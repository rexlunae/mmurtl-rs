pub use super::mmu::{map_stack_page as map, stack_page_mapped as mapped, unmap_stack_page as unmap};
pub use super::mmu::detach_stack_tables as detach_empty;
pub fn invalidate() { unsafe { core::arch::asm!("dsb ishst", "tlbi vmalle1is", "dsb ish", "isb"); } }
pub fn interrupts_enabled() -> bool {
    let daif: u64; unsafe { core::arch::asm!("mrs {}, daif",out(reg) daif,options(nomem,nostack)); }
    daif & (1 << 7) == 0
}
pub fn on_emergency_stack() -> bool {
    let sp: u64; unsafe { core::arch::asm!("mov {}, sp",out(reg) sp,options(nomem,nostack)); }
    let top = unsafe { core::ptr::addr_of!(ARM64_STACK_EMERGENCY[super::cpu_index()]).read_volatile() };
    top >= 32768 && (top - 32768..top).contains(&sp)
}
#[no_mangle]
static mut ARM64_STACK_EMERGENCY: [u64; crate::scheduler::MAX_CPUS] = [0; crate::scheduler::MAX_CPUS];
#[no_mangle]
static mut ARM64_SCS_EMERGENCY: [u64; crate::scheduler::MAX_CPUS] = [0; crate::scheduler::MAX_CPUS];
pub fn init_emergency() {
    for cpu in 0..crate::scheduler::MAX_CPUS {
        let stack = crate::memory::stacks::allocate();
        unsafe { ARM64_STACK_EMERGENCY[cpu] = stack.virt + stack.size; }
        let shadow = crate::memory::stacks::allocate();
        unsafe { ARM64_SCS_EMERGENCY[cpu] = shadow.virt; }
    }
}
pub fn emergency_byte(byte: u8) { super::serial::emergency_byte(byte); }
core::arch::global_asm!(r#"
.text
.global arm64_stack_overflow
arm64_stack_overflow:
    bti jc
    msr daifset, #15
    mrs x0, tpidr_el1
    adrp x1, ARM64_STACK_EMERGENCY
    add x1, x1, :lo12:ARM64_STACK_EMERGENCY
    ldr x1, [x1, x0, lsl #3]
    mov sp, x1
    adrp x1, ARM64_SCS_EMERGENCY
    add x1, x1, :lo12:ARM64_SCS_EMERGENCY
    ldr x18, [x1, x0, lsl #3]
    adrp x1, ARM64_PAN_MODE
    add x1, x1, :lo12:ARM64_PAN_MODE
    ldarb w1, [x1]
    cmp w1, #1
    b.eq .Loverflow_hardware_pan
    cmp w1, #2
    b.ne .Loverflow_pan_closed
    adrp x1, ARM64_EMPTY_TTBR0
    ldr x1, [x1, :lo12:ARM64_EMPTY_TTBR0]
    msr ttbr0_el1, x1
    isb
    b .Loverflow_pan_closed
.Loverflow_hardware_pan:
    .inst 0xd500419f
    isb
.Loverflow_pan_closed:
    bl arm64_restore_kernel_pac
    b kernel_stack_fatal
"#);
