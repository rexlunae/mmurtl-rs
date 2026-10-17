//! Regression: a relocated pointer page must be read-only at runtime.
#![no_std]
#![no_main]
mmurtl::entry!(main);
core::arch::global_asm!(r#"
.section .data.rel.ro,"aw"
.balign 8
.global user_relro_target
user_relro_target:
.quad _start
.text
"#);
fn main() {
    mmurtl::log("[USER RELRO] attempting write to relocated pointer page");
    #[cfg(target_arch="x86_64")]
    unsafe { core::arch::asm!("lea rax, [rip + user_relro_target]", "mov qword ptr [rax], 0",out("rax") _); }
    #[cfg(target_arch="aarch64")]
    unsafe { core::arch::asm!("adrp x0, user_relro_target", "add x0, x0, :lo12:user_relro_target", "str xzr, [x0]",out("x0") _); }
    mmurtl::log("[USER RELRO] ERROR: write succeeded");
}
