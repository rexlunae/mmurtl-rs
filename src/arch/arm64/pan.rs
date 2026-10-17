//! Privileged Access Never. The uniform entry mode uses hardware PAN when
//! every admitted CPU supports it, otherwise an empty TTBR0 with reserved ASID
//! zero. Only bounded, IRQ-masked user-copy windows may reopen user access.
use core::arch::asm;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use crate::security::feature as F;
const HW: u8 = 1;
const SW: u8 = 2;
#[no_mangle]
pub static ARM64_PAN_MODE: AtomicU8 = AtomicU8::new(0);
#[no_mangle]
pub static ARM64_EMPTY_TTBR0: AtomicU64 = AtomicU64::new(0);
static CONFIGURED: [AtomicBool; crate::scheduler::MAX_CPUS] = [const { AtomicBool::new(false) }; crate::scheduler::MAX_CPUS];

pub fn software() -> bool { ARM64_PAN_MODE.load(Ordering::Acquire) == SW }
pub fn enabled() -> bool { ARM64_PAN_MODE.load(Ordering::Acquire) != 0 }
pub fn init() {
    crate::arch::without_interrupts(|| {
        ARM64_EMPTY_TTBR0.store(super::mmu::empty_user_root(), Ordering::Release);
        ARM64_PAN_MODE.store(if crate::security::common().features & F::PAN != 0 { HW } else { SW }, Ordering::Release);
        apply_current(); end();
    });
    crate::serial::write_line(if software() { "[PAN] software PAN: kernel TTBR0 empty; guarded user copies" }
        else { "[PAN] hardware PAN: SPAN=0, PAN=1; EPAN enabled where supported" });
}
pub fn apply_current() {
    let mode = ARM64_PAN_MODE.load(Ordering::Acquire);
    if mode == 0 { return; }
    let cpu = super::cpu_index();
    if CONFIGURED[cpu].load(Ordering::Acquire) { return; }
    let caps = crate::security::caps(cpu).unwrap_or_default();
    unsafe {
        // UAO would turn LDTR/STTR into privileged accesses. Never inherit it.
        if caps.features & F::UAO != 0 { asm!(".inst 0xd500407f", options(nostack)); } // UAO=0
        if mode == HW {
            assert!(caps.features & F::PAN != 0);
            let mut sctlr: u64; asm!("mrs {}, sctlr_el1", out(reg) sctlr);
            sctlr &= !(1 << 23); // SPAN=0: exception entry sets PAN
            if caps.features & F::EPAN != 0 { sctlr |= 1 << 57; }
            asm!("msr sctlr_el1, {}", "isb", ".inst 0xd500419f", "isb", in(reg) sctlr); // PAN=1
        } else if caps.features & F::PAN != 0 {
            // Software mode may be chosen for a heterogeneous machine.
            // Do not let inherited hardware PAN interfere with its windows.
            asm!(".inst 0xd500409f", "isb", options(nostack)); // PAN=0
        }
    }
    CONFIGURED[cpu].store(true, Ordering::Release);
}
fn assert_irq_masked() {
    let daif: u64; unsafe { asm!("mrs {}, daif", out(reg) daif, options(nomem, nostack)); }
    assert!(daif & (1 << 7) != 0, "PAN window must not be preemptible");
}
pub fn begin() {
    let mode = ARM64_PAN_MODE.load(Ordering::Acquire);
    if mode == 0 { return; }
    assert_irq_masked();
    unsafe {
        if mode == HW { asm!(".inst 0xd500409f", "isb", options(nostack)); }
        else { asm!("msr ttbr0_el1, {}", "isb", in(reg) super::mmu::saved_user_ttbr0(), options(nostack)); }
    }
}
pub fn end() {
    let mode = ARM64_PAN_MODE.load(Ordering::Acquire);
    if mode == 0 { return; }
    assert_irq_masked();
    unsafe {
        if mode == HW { asm!(".inst 0xd500419f", "isb", options(nostack)); }
        else { asm!("msr ttbr0_el1, {}", "isb", in(reg) ARM64_EMPTY_TTBR0.load(Ordering::Acquire), options(nostack)); }
    }
}
/// Last Rust action before restoring registers/ERET. Kernel resumes stay
/// protected; a user resume alone restores its private TTBR0 in software mode.
pub fn prepare_return(frame: &mut super::TaskContext) {
    apply_current();
    let mode = ARM64_PAN_MODE.load(Ordering::Acquire);
    frame.spsr = crate::security::arm_pan::return_spsr(frame.spsr, mode == HW);
    if mode == SW {
        let root = if frame.spsr & 15 == 0 { super::mmu::saved_user_ttbr0() } else { ARM64_EMPTY_TTBR0.load(Ordering::Acquire) };
        unsafe { asm!("msr ttbr0_el1, {}", "isb", in(reg) root, options(nostack)); }
    }
}

pub fn assert_closed() {
    match ARM64_PAN_MODE.load(Ordering::Acquire) {
        HW => {
            let value: u64;
            unsafe { asm!("mrs {}, S3_0_C4_C2_3", out(reg) value, options(nomem, nostack)); }
            assert!(value & (1 << 22) != 0, "kernel entered with PAN clear");
        }
        SW => {
            let value: u64; unsafe { asm!("mrs {}, ttbr0_el1", out(reg) value, options(nomem, nostack)); }
            assert_eq!(value, ARM64_EMPTY_TTBR0.load(Ordering::Acquire), "kernel entered with user TTBR0 open");
        }
        _ => {}
    }
}
static TEST_TARGET: AtomicU64 = AtomicU64::new(0);
static KERNEL_BYTE: u8 = 0x5a;
core::arch::global_asm!(r#"
.text
.global pan_read_probe, pan_write_probe
pan_read_probe:
    bti jc
.Lpan_read_fault:
    ldrb w1, [x0]
    mov x0, #0
    ret
    dsb nsh
    isb
.Lpan_read_denied:
    mov x0, #1
    ret
    dsb nsh
    isb
pan_write_probe:
    bti jc
.Lpan_write_fault:
    strb w1, [x0]
    mov x0, #0
    ret
    dsb nsh
    isb
.Lpan_write_denied:
    mov x0, #1
    ret
    dsb nsh
    isb
.pushsection .rodata.pan_ex_table, "a"
.balign 8
.global pan_ex_table
pan_ex_table:
    .quad .Lpan_read_fault - .
    .quad .Lpan_read_denied - .
    .quad .Lpan_write_fault - .
    .quad .Lpan_write_denied - .
.popsection
"#);
extern "C" {
    fn pan_read_probe(pointer: u64) -> u64;
    fn pan_write_probe(pointer: u64, byte: u64) -> u64;
    static pan_ex_table: [i64; 4];
}
pub fn test_fixup(esr: u64, pc: u64, far: u64) -> Option<u64> {
    if super::cpu_index() != 0 || esr >> 26 != 0x25 || far == 0 || TEST_TARGET.load(Ordering::Acquire) != far { return None; }
    let fsc = esr & 63;
    if !(if software() { (4..=7).contains(&fsc) } else { (13..=15).contains(&fsc) }) { return None; }
    let entries = core::ptr::addr_of!(pan_ex_table).cast::<i64>();
    for i in [0, 2] {
        unsafe {
            let from = entries.add(i); let to = from.add(1);
            if pc == (from as u64).wrapping_add_signed(from.read()) {
                return Some((to as u64).wrapping_add_signed(to.read()));
            }
        }
    }
    None
}
unsafe fn denied(pointer: u64, write: bool) {
    TEST_TARGET.store(pointer, Ordering::Release);
    let result = if write { pan_write_probe(pointer, 0) } else { pan_read_probe(pointer) };
    TEST_TARGET.store(0, Ordering::Release);
    assert_eq!(result, 1, "unguarded EL1 user access was allowed");
}
pub fn self_test() {
    let caps = crate::security::caps(0).unwrap_or_default();
    let image = match crate::memory::user::load_program(&0xd65f03c0u32.to_le_bytes()) {
        Ok(image) => image,
        Err(_) => { crate::serial::write_line("[PAN] probes skipped: out of memory"); return; }
    };
    crate::arch::without_interrupts(|| unsafe {
        if caps.features & F::UAO != 0 {
            asm!(".inst 0xd500417f", options(nostack)); // emulate inherited UAO=1
            CONFIGURED[0].store(false, Ordering::Release); apply_current();
        }
        super::mmu::switch_address_space(0, image.space);
        assert_closed();
        let stack = image.stack_top - 8;
        denied(stack, false); denied(stack, true);
        // The explicit window permits privileged accesses to this RW page.
        begin(); assert_eq!(pan_read_probe(stack), 0); assert_eq!(pan_write_probe(stack, 0), 0); end();
        assert_closed();
        let mut byte = 0;
        assert_eq!(super::uaccess::copy_from_user(&mut byte, stack, 1), 0);
        assert_eq!(super::uaccess::copy_to_user(stack, [0u8].as_ptr(), 1), 0);
        assert_eq!(super::uaccess::copy_from_user(&mut byte, core::ptr::addr_of!(KERNEL_BYTE) as u64, 1), 1,
            "LDTR inherited privileged UAO access");
        if software() || caps.features & F::EPAN != 0 {
            super::mmu::make_test_execute_only(image.space, image.entry);
            denied(image.entry, false);
            assert_eq!(super::uaccess::copy_from_user(&mut byte, image.entry, 1), 1, "LDTR read execute-only data");
        }
        // A kernel exception must not return with its interrupted access
        // window open. Only the boot idle task exists on this CPU here.
        begin(); asm!("svc #0"); assert_closed(); end();
        super::mmu::switch_address_space(0, 0);
    });
    crate::memory::user::release(image.space, image.slot);
    crate::serial::write_line("[PAN] fault probes passed (unguarded read/write, guarded copies, kernel pointer, exception return)");
    if !software() && caps.features & F::EPAN != 0 { crate::serial::write_line("[PAN] EPAN execute-only read denied"); }
}
