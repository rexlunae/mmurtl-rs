pub fn initialize()->bool {
    let active=crate::security::common().features & crate::security::feature::BTI!=0;
    if active {super::mmu::guard_kernel_text();}
    active
}
pub fn status(active:bool)->&'static str {if active {"[CFI] BTI guarded kernel text active; strict PAC landing compatibility applied per CPU"} else {"[CFI] BTI unavailable on common CPU set; KCFI remains active"}}
pub fn enable_current() {
    unsafe {
        let mut sctlr:u64;core::arch::asm!("mrs {}, sctlr_el1",out(reg) sctlr,options(nomem,nostack));
        // GP PTEs enforce BTI. BT1 makes implicit PAC landing pads behave
        // as BTI c rather than admitting arbitrary indirect-jump BTYPE.
        sctlr|=crate::security::cfi_policy::SCTLR_BT1;
        core::arch::asm!("msr sctlr_el1, {}","isb",in(reg) sctlr,options(nostack));
        let actual:u64;core::arch::asm!("mrs {}, sctlr_el1",out(reg) actual,options(nomem,nostack));
        assert_ne!(actual&crate::security::cfi_policy::SCTLR_BT1,0);
    }
}
#[cfg(any(feature="test-bti",feature="test-ibt"))]
core::arch::global_asm!(r#"
.text
.global cfi_landing_probe
cfi_landing_probe:
    bti jc
    adr x16, cfi_non_landing
    br x16
    dsb nsh
    isb
.global cfi_non_landing
cfi_non_landing:
    nop
    b kernel_cfi_probe_escaped
"#);
#[cfg(any(feature="test-bti",feature="test-ibt"))]
pub unsafe fn test_landing()->! {
    unsafe extern "C" {fn cfi_landing_probe()->!;}
    cfi_landing_probe()
}
