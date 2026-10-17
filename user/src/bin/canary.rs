//! Deliberate regression: corrupt a compiler-protected frame and verify the
//! runtime's guard failure exits before the overwritten return address is used.
#![no_std]
#![no_main]
mmurtl::entry!(main);
#[cfg(target_arch="x86_64")]
core::arch::global_asm!(r#"
.text
.global user_smash_bytes
user_smash_bytes:
    mov rcx, 128
    mov al, 0xa5
    rep stosb
    ret
    int3
"#);
#[cfg(target_arch="aarch64")]
core::arch::global_asm!(r#"
.text
.global user_smash_bytes
user_smash_bytes:
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
#[inline(never)]
fn protected_frame() {
    let mut bytes=[0u8;64];
    unsafe extern "C" { fn user_smash_bytes(bytes:*mut u8); }
    unsafe { user_smash_bytes(bytes.as_mut_ptr()); }
    core::hint::black_box(&bytes);
}
fn main() {
    mmurtl::log("[USER CANARY] deliberate frame corruption");
    protected_frame();
    mmurtl::log("[USER CANARY] ERROR: corrupted frame returned");
}
