//! Global Descriptor Table — Required in long mode for
//!   - Kernel vs user segments (CS, SS, DS)
//!   - TSS (Task State Segment) for IST stacks
//!   - User mode segments (for future userspace)

use x86_64::structures::gdt::{GlobalDescriptorTable, Descriptor};
use x86_64::structures::tss::TaskStateSegment;
use x86_64::structures::gdt::SegmentSelector;
use x86_64::VirtAddr;
use spin::Lazy;

/// Stack size for double-fault IST
pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;
const STACK_SIZE: usize = 4096 * 16; // 64KB

/// Double-fault IST stack
static DOUBLE_FAULT_STACK: [u8; STACK_SIZE] = [0; STACK_SIZE];

/// Task State Segment
static TSS: Lazy<TaskStateSegment> = Lazy::new(|| {
    let mut tss = TaskStateSegment::new();
    let stack_top = &DOUBLE_FAULT_STACK as *const _ as u64 + STACK_SIZE as u64;
    tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = VirtAddr::new(stack_top);
    tss
});

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
    let tss_entry = gdt.add_entry(Descriptor::tss_segment(&TSS));
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
}
