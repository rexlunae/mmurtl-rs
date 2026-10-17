//! Shared boot PAC key and algorithm compatibility for migrating frames.
use core::sync::atomic::{AtomicBool,Ordering};
#[no_mangle]
static ARM64_PAC_REQUESTED:AtomicBool=AtomicBool::new(false);
#[no_mangle]
static mut ARM64_PAC_SIGNATURE:u64=0;
#[no_mangle]
static mut ARM64_PAC_KEY:[u64;2]=[0;2];
#[no_mangle]
static ARM64_PAC_CPU_ACTIVE:[AtomicBool;crate::scheduler::MAX_CPUS]=[const {AtomicBool::new(false)};crate::scheduler::MAX_CPUS];
pub fn init() {
    let a:u64;let b:u64;
    unsafe {core::arch::asm!("mrs {}, id_aa64isar1_el1","mrs {}, id_aa64isar2_el1",out(reg) a,out(reg) b,options(nomem,nostack));}
    let signature=crate::security::return_policy::pac_signature(a,b);
    let mut key=[0u8;16];
    if let Some(signature)=signature.filter(|_|crate::random::fill(&mut key).is_ok()) {
        unsafe {ARM64_PAC_SIGNATURE=signature;ARM64_PAC_KEY=[u64::from_le_bytes(key[..8].try_into().unwrap()),u64::from_le_bytes(key[8..].try_into().unwrap())];}
        crate::random::crypto::erase(&mut key);
        ARM64_PAC_REQUESTED.store(true,Ordering::Release);
        crate::serial::write_line("[RETURN] PAC-ret selected; compatible CPUs and fresh runtime stacks required");
    } else {crate::serial::write_line("[RETURN] PAC unavailable or entropy insufficient; guarded shadow call stacks remain active");}
}
pub fn compatible(cpu:usize)->bool {
    !ARM64_PAC_REQUESTED.load(Ordering::Acquire)||ARM64_PAC_CPU_ACTIVE[cpu].load(Ordering::Acquire)
}
pub fn audit() {
    if !ARM64_PAC_REQUESTED.load(Ordering::Acquire) {return;}
    let admitted=crate::security::freeze();
    for cpu in 0..crate::scheduler::MAX_CPUS {
        if admitted&(1<<cpu)!=0 {assert!(ARM64_PAC_CPU_ACTIVE[cpu].load(Ordering::Acquire));}
    }
    let lo:u64;let hi:u64;let control:u64;
    unsafe {
        core::arch::asm!("mrs {}, S3_0_C2_C1_0","mrs {}, S3_0_C2_C1_1","mrs {}, sctlr_el1",out(reg) lo,out(reg) hi,out(reg) control,options(nomem,nostack));
        assert!(lo==ARM64_PAC_KEY[0]&&hi==ARM64_PAC_KEY[1],"kernel PAC key mismatch");
    }
    assert_ne!(control&(1<<31),0);
    crate::serial::write_line("[RETURN] PAC-ret active on every admitted CPU; user key isolated");
}
#[cfg(feature="test-pac")]
pub fn test()->! {
    if !ARM64_PAC_CPU_ACTIVE[super::cpu_index()].load(Ordering::Acquire) {
        crate::serial::write_line("[CFI TEST] hardware landing enforcement unavailable; positive test skipped");
        loop {super::halt();}
    }
    unsafe extern "C" {fn kernel_pac_probe();}
    unsafe {kernel_pac_probe();}
    panic!("corrupted PAC authenticated");
}
#[cfg(feature="test-pac")]
#[no_mangle]
extern "C" fn kernel_pac_failed()->! {crate::cfi::fatal(b"PAC authentication",0,None)}
#[cfg(feature="test-pac")]
core::arch::global_asm!(r#"
.text
.global kernel_pac_probe
kernel_pac_probe:
    bti jc
    mov x9, x30
    paciasp
    eor x30, x30, #(1 << 55)
    autiasp
    cmp x30, x9
    b.ne kernel_pac_failed
    ret
    dsb nsh
    isb
"#);
core::arch::global_asm!(r#"
.text
.global arm64_enable_boot_pac
arm64_enable_boot_pac:
    bti jc
    adrp x1, ARM64_PAC_REQUESTED
    add x1, x1, :lo12:ARM64_PAC_REQUESTED
    ldarb w1, [x1]
    cbz w1, .Lpac_boot_done
    mrs x1, id_aa64isar1_el1
    ubfx x1, x1, #4, #8
    mrs x2, id_aa64isar2_el1
    ubfx x2, x2, #12, #4
    orr x1, x1, x2, lsl #8
    adrp x2, ARM64_PAC_SIGNATURE
    ldr x2, [x2, :lo12:ARM64_PAC_SIGNATURE]
    cmp x1, x2
    b.ne .Lpac_boot_done
    mrs x1, tpidr_el1
    adrp x2, ARM64_PAC_CPU_ACTIVE
    add x2, x2, :lo12:ARM64_PAC_CPU_ACTIVE
    mov w3, #1
    strb w3, [x2, x1]
    b arm64_restore_kernel_pac
.Lpac_boot_done:
    ret
    dsb nsh
    isb
.global arm64_restore_kernel_pac
arm64_restore_kernel_pac:
    bti jc
    mrs x1, tpidr_el1
    adrp x2, ARM64_PAC_CPU_ACTIVE
    add x2, x2, :lo12:ARM64_PAC_CPU_ACTIVE
    ldrb w1, [x2, x1]
    cbz w1, .Lpac_restore_done
    adrp x1, ARM64_PAC_KEY
    add x1, x1, :lo12:ARM64_PAC_KEY
    ldp x2, x3, [x1]
    msr S3_0_C2_C1_0, x2
    msr S3_0_C2_C1_1, x3
    mrs x1, sctlr_el1
    orr x1, x1, #(1 << 31)
    msr sctlr_el1, x1
    isb
.Lpac_restore_done:
    ret
    dsb nsh
    isb
.global arm64_clear_user_pac
arm64_clear_user_pac:
    bti jc
    mrs x1, tpidr_el1
    adrp x2, ARM64_PAC_CPU_ACTIVE
    add x2, x2, :lo12:ARM64_PAC_CPU_ACTIVE
    ldrb w1, [x2, x1]
    cbz w1, .Lpac_user_done
    // No user PAC ABI yet. Never expose the kernel key as an EL0 oracle.
    msr S3_0_C2_C1_0, xzr
    msr S3_0_C2_C1_1, xzr
    mrs x1, sctlr_el1
    bic x1, x1, #(1 << 31)
    msr sctlr_el1, x1
    isb
.Lpac_user_done:
    ret
    dsb nsh
    isb
"#);
