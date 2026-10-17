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
pub const NMI_IST_INDEX: u16 = 1;
pub const MACHINE_CHECK_IST_INDEX: u16 = 2;
pub const DEBUG_IST_INDEX: u16 = 3;
// Early BSP stacks remain usable before page-table/allocator setup.
static mut EARLY_IST: [IstStack; 3] = [const { IstStack([0; STACK_SIZE]) }; 3];
const STACK_SIZE: usize = 4096 * 16; // 64KB

/// Backing storage for an IST stack (the CPU writes the exception frame
/// at its top, so it must be writable and 16-byte aligned)
#[repr(C, align(16))]
struct IstStack([u8; STACK_SIZE]);

/// The BSP's double-fault IST stack. It must be a `static mut`: an
/// immutable static lands in `.rodata`, which the bootloader maps
/// read-only, and the CPU's frame push on a double fault would then
/// fault again and triple-fault the machine. (APs allocate theirs from
/// the heap in `init_ap`.)
static mut DOUBLE_FAULT_STACK: IstStack = IstStack([0; STACK_SIZE]);

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
        let stack_top = core::ptr::addr_of!(DOUBLE_FAULT_STACK) as u64 + STACK_SIZE as u64;
        t.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = VirtAddr::new(stack_top);
        for i in 0..3 {
            let base = core::ptr::addr_of!(EARLY_IST).cast::<IstStack>().add(i) as u64;
            t.interrupt_stack_table[i + 1] = VirtAddr::new(base + STACK_SIZE as u64);
        }
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
/// Each AP gets its own GDT, TSS, and guarded emergency stacks, retained
/// for the machine's lifetime. A TSS
/// cannot be shared: `ltr` marks its descriptor busy, so a second CPU
/// loading the same descriptor would fault.
pub fn init_ap(cpu: usize) {
    use alloc::boxed::Box;
    use x86_64::instructions::segmentation::{CS, DS, ES, SS, Segment};
    use x86_64::instructions::tables::load_tss;

    let tss: *mut TaskStateSegment = Box::into_raw(Box::new(TaskStateSegment::new()));
    install_guarded_stacks(cpu, tss);

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

/// Reserve four separate supervisor RW/NX stacks, each preceded by an
/// unmapped guard. Never reclaimed: they belong to the CPU, not a task.
fn install_guarded_stacks(cpu: usize, tss: *mut TaskStateSegment) {
    use super::page_table::{self, BumpFrameAllocator};
    use x86_64::{PhysAddr, structures::paging::{Page, PhysFrame, page_table::PageTableFlags as F}};
    static BASE: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
    let mut base = BASE.load(Ordering::Acquire);
    if cpu == 0 && base == 0 {
        base = crate::security::kaslr::amd_ist(crate::random::early_u64());
        BASE.store(base, Ordering::Release);
    }
    assert!(base != 0, "BSP must place emergency stacks before AP startup");
    const PAGES: u64 = 8; // 32 KiB, plus one guard page per stack
    let mut tops = [0u64; 4];
    crate::memory::heap::with_frame_allocator(|fa| {
        let mut adapter = BumpFrameAllocator::new(fa);
        for (slot, top) in tops.iter_mut().enumerate() {
            let guard = base + (cpu as u64 * 4 + slot as u64) * (PAGES + 1) * 4096;
            assert!(page_table::query_page(VirtAddr::new(guard)).is_none());
            for page in 1..=PAGES {
                let pa = fa.allocate_zeroed_frame().expect("OOM: emergency stack");
                unsafe {
                    page_table::map_page(Page::containing_address(VirtAddr::new(guard + page * 4096)),
                        PhysFrame::containing_address(PhysAddr::new(pa)), F::WRITABLE | F::NO_EXECUTE,
                        &mut adapter).expect("map emergency stack");
                }
            }
            *top = guard + (PAGES + 1) * 4096;
            let flags = page_table::query_page(VirtAddr::new(*top - 1)).expect("missing IST mapping");
            assert!(flags.contains(F::WRITABLE | F::NO_EXECUTE) && !flags.contains(F::USER_ACCESSIBLE));
        }
    }).expect("frame allocator not initialized");
    for (slot, top) in tops.into_iter().enumerate() {
        unsafe { (*tss).interrupt_stack_table[slot] = VirtAddr::new(top); }
    }
    use core::fmt::Write;
    let mut line: heapless::String<80> = heapless::String::new();
    let _ = write!(line, "[IST] CPU{}: guarded RW/NX #DF #NMI #MC #DB stacks\n", cpu);
    crate::serial::write_str(&line);
}

/// Replace the early BSP stacks once the allocator and physmap exist,
/// before starting APs or user tasks.
pub fn protect_bsp_stacks() {
    install_guarded_stacks(0, core::ptr::addr_of_mut!(BSP_TSS));
}
