pub use super::mmu::{map_stack_page as map, stack_page_mapped as mapped, unmap_stack_page as unmap};
pub use super::mmu::detach_stack_tables as detach_empty;
pub fn invalidate() { unsafe { core::arch::asm!("dsb ishst", "tlbi vmalle1is", "dsb ish", "isb"); } }
#[no_mangle]
static mut ARM64_STACK_EMERGENCY: [u64; crate::scheduler::MAX_CPUS] = [0; crate::scheduler::MAX_CPUS];
pub fn init_emergency() {
    for cpu in 0..crate::scheduler::MAX_CPUS {
        let stack = crate::memory::stacks::allocate();
        unsafe { ARM64_STACK_EMERGENCY[cpu] = stack.virt + stack.size; }
    }
}
pub fn emergency_byte(byte: u8) { super::serial::emergency_byte(byte); }
core::arch::global_asm!(r#"
.text
.global arm64_stack_overflow
arm64_stack_overflow:
    msr daifset, #15
    mrs x0, tpidr_el1
    adrp x1, ARM64_STACK_EMERGENCY
    add x1, x1, :lo12:ARM64_STACK_EMERGENCY
    ldr x1, [x1, x0, lsl #3]
    mov sp, x1
    b kernel_stack_fatal
"#);
