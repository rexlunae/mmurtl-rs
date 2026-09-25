//! Global Descriptor Table — Required in long mode for
//!   - Kernel vs user segments (CS, SS, DS)
//!   - TSS (Task State Segment) for IST stacks
//!   - User mode segments (for future userspace)

use x86_64::structures::gdt::{GlobalDescriptorTable, Descriptor};
use x86_64::structures::tss::TaskStateSegment;
use x86_64::structures::gdt::SegmentSelector;
use x86_64::VirtAddr;
use core::sync::atomic::{AtomicPtr, Ordering};
use spin::Lazy;

/// Ring-3 code selector (GDT index 4, RPL 3)
pub const USER_CS: u64 = 0x20 | 3;
/// Ring-3 data/stack selector (GDT index 3, RPL 3)
pub const USER_SS: u64 = 0x18 | 3;

/// Each CPU's TSS, by CPU index. `rsp0` must be rewritten on every switch
/// to a user task: it is the stack the CPU loads when an interrupt or
/// `int 0x80` arrives from ring 3.
static TSS_BY_CPU: [AtomicPtr<TaskStateSegment>; crate::scheduler::MAX_CPUS] = {
    const NULL: AtomicPtr<TaskStateSegment> = AtomicPtr::new(core::ptr::null_mut());
    [NULL; crate::scheduler::MAX_CPUS]
};

/// Point `cpu`'s TSS.RSP0 at `stack_top` (the incoming user task's kernel
/// stack). Called by the scheduler, on that CPU, with the lock held.
pub fn set_kernel_stack(cpu: usize, stack_top: u64) {
    let tss = TSS_BY_CPU[cpu].load(Ordering::Relaxed);
    if !tss.is_null() {
        unsafe {
            core::ptr::addr_of_mut!((*tss).privilege_stack_table[0])
                .write_volatile(VirtAddr::new(stack_top));
        }
    }
}

/// Stack size for double-fault IST
pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;
const STACK_SIZE: usize = 4096 * 16; // 64KB

/// Double-fault IST stack
static DOUBLE_FAULT_STACK: [u8; STACK_SIZE] = [0; STACK_SIZE];

/// The BSP's Task State Segment. Mutable (RSP0 changes per user task),
/// so it lives in a `static mut` rather than behind a shared reference;
/// the BSP GDT is built before the heap exists.
static mut BSP_TSS: TaskStateSegment = TaskStateSegment::new();

/// GDT with kernel, user, and TSS segments
static GDT: Lazy<InnerGdt> = Lazy::new(|| {
    let mut gdt = GlobalDescriptorTable::new();
    let kernel_code = gdt.add_entry(Descriptor::kernel_code_segment());
    let kernel_data = gdt.add_entry(Descriptor::kernel_data_segment());
    // user_data must come before user_code for SYSRET:
    //   SS = (STAR[63:48] + 8) | 3 → user_data
    //   CS = (STAR[63:48] + 16) | 3 → user_code
    let user_data = gdt.add_entry(Descriptor::user_data_segment());
    let user_code = gdt.add_entry(Descriptor::user_code_segment());
    let tss: &'static TaskStateSegment = unsafe {
        let t = &mut *core::ptr::addr_of_mut!(BSP_TSS);
        let stack_top = &DOUBLE_FAULT_STACK as *const _ as u64 + STACK_SIZE as u64;
        t.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = VirtAddr::new(stack_top);
        &*core::ptr::addr_of!(BSP_TSS)
    };
    let tss_entry = gdt.add_entry(Descriptor::tss_segment(tss));
    InnerGdt { gdt, selectors: Selectors { kernel_code, kernel_data, user_code, user_data, tss_entry } }
});

/// Inner struct holding both GDT and selectors
struct InnerGdt {
    gdt: GlobalDescriptorTable,
    selectors: Selectors,
}

/// GDT selector values for easy reference
pub struct Selectors {
    pub kernel_code: SegmentSelector,
    pub kernel_data: SegmentSelector,
    pub user_code: SegmentSelector,
    pub user_data: SegmentSelector,
    pub tss_entry: SegmentSelector,
}

/// Initialize an application processor's GDT.
///
/// Each AP gets its own GDT, TSS, and double-fault IST stack (allocated
/// from the heap and leaked — they live for the machine's lifetime). A TSS
/// cannot be shared: `ltr` marks its descriptor busy, so a second CPU
/// loading the same descriptor would fault.
pub fn init_ap(cpu: usize) {
    use alloc::boxed::Box;
    use alloc::vec;
    use x86_64::instructions::segmentation::{CS, DS, ES, SS, Segment};
    use x86_64::instructions::tables::load_tss;

    // Per-CPU double-fault IST stack
    let ist_stack: &'static mut [u8] = Box::leak(vec![0u8; STACK_SIZE].into_boxed_slice());
    let stack_top = ist_stack.as_ptr() as u64 + ist_stack.len() as u64;

    // Leaked as a raw pointer: the GDT descriptor needs a 'static shared
    // reference, and the scheduler later rewrites RSP0 through the pointer
    let tss: *mut TaskStateSegment = Box::into_raw(Box::new(TaskStateSegment::new()));
    unsafe {
        (*tss).interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = VirtAddr::new(stack_top);
    }

    let gdt: &'static mut GlobalDescriptorTable = Box::leak(Box::new(GlobalDescriptorTable::new()));
    let kernel_code = gdt.add_entry(Descriptor::kernel_code_segment());
    let kernel_data = gdt.add_entry(Descriptor::kernel_data_segment());
    let _user_data = gdt.add_entry(Descriptor::user_data_segment());
    let _user_code = gdt.add_entry(Descriptor::user_code_segment());
    let tss_entry = gdt.add_entry(Descriptor::tss_segment(unsafe { &*tss }));

    gdt.load();
    unsafe {
        CS::set_reg(kernel_code);
        SS::set_reg(kernel_data);
        DS::set_reg(kernel_data);
        ES::set_reg(kernel_data);
        load_tss(tss_entry);
    }
    TSS_BY_CPU[cpu].store(tss, Ordering::Release);
}

/// Initialize the GDT — call once at boot
pub fn init() {
    use x86_64::instructions::segmentation::{CS, Segment};
    use x86_64::instructions::tables::load_tss;

    GDT.gdt.load();

    // Reload CS with kernel code segment
    unsafe {
        CS::set_reg(GDT.selectors.kernel_code);
        load_tss(GDT.selectors.tss_entry);
    }
    TSS_BY_CPU[0].store(core::ptr::addr_of_mut!(BSP_TSS), Ordering::Release);
}
