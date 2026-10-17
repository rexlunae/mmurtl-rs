//! Kernel compiler stack guards. The bootstrap guard is replaced once,
//! before secondary CPUs start. The BSP bootstrap frames never return;
//! runtime execution begins on a fresh guarded stack.
#[no_mangle]
pub static mut __stack_chk_guard: u64 = 0x00976f4c5a381200;

#[no_mangle]
pub extern "C" fn __stack_chk_fail() -> ! {
    crate::cfi::fatal(b"stack canary", 0, None)
}

#[no_mangle]
extern "C" fn kernel_guard_draw() -> u64 {
    let guard = crate::random::u64().unwrap_or_else(|_| crate::random::early_u64());
    if !crate::random::ready() {
        crate::serial::write_line("[RETURN] WARNING: weak bootstrap entropy; canary secrecy is not assured");
    }
    crate::security::return_policy::canary(guard)
}

/// This function must stay naked: the protected draw helper returns under
/// the old guard before assembly publishes the new one. Only the BSP's
/// nonreturning bootstrap invokes this, with IRQs masked and no APs alive.
#[unsafe(naked)]
pub unsafe extern "C" fn seed_boot_guard() {
    #[cfg(target_arch="x86_64")]
    core::arch::naked_asm!(
        "endbr64", "push rbx", "call {draw}",
        "mov [rip + __stack_chk_guard], rax", "pop rbx", "jmp __x86_return_thunk", "int3",
        draw=sym kernel_guard_draw,
    );
    #[cfg(target_arch="aarch64")]
    core::arch::naked_asm!(
        "bti jc", "stp x29, x30, [sp, #-16]!", "bl {draw}",
        "adrp x1, __stack_chk_guard", "str x0, [x1, :lo12:__stack_chk_guard]",
        "ldp x29, x30, [sp], #16", "ret", "dsb nsh", "isb",
        draw=sym kernel_guard_draw,
    );
}

#[cfg(all(feature="test-kernel-canary",target_arch="x86_64"))]
core::arch::global_asm!(r#"
.text
.global kernel_smash_bytes
kernel_smash_bytes:
    endbr64
    mov rcx, 128
    mov al, 0xa5
    rep stosb
    jmp __x86_return_thunk
    int3
"#);
#[cfg(all(feature="test-kernel-canary",target_arch="aarch64"))]
core::arch::global_asm!(r#"
.text
.global kernel_smash_bytes
kernel_smash_bytes:
    bti jc
    mov x1, #128
    mov w2, #0xa5
1:
    strb w2, [x0], #1
    subs x1, x1, #1
    b.ne 1b
    ret
    dsb nsh
    isb
"#);
#[cfg(feature="test-kernel-canary")]
#[inline(never)]
fn protected_frame() {
    let mut bytes=[0u8;64];
    unsafe extern "C" {fn kernel_smash_bytes(bytes:*mut u8);}
    unsafe {kernel_smash_bytes(bytes.as_mut_ptr());}
    core::hint::black_box(&bytes);
}
#[cfg(feature="test-kernel-canary")]
pub fn test_canary()->! {
    protected_frame();
    panic!("corrupted kernel frame returned");
}
