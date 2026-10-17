//! Read-only firmware memory-encryption status. Enabling encryption after
//! boot would invalidate existing plaintext mappings and is not safe here.
pub fn report() {
    use core::arch::x86_64::{__cpuid, __cpuid_count};
    use crate::serial;
    unsafe {
        let base = __cpuid(0);
        // Intel SDM: TME CPUID.7.0:ECX[13] gates IA32_TME_ACTIVATE.
        if base.eax >= 7 && __cpuid_count(7, 0).ecx & (1 << 13) != 0 {
            let (lo, hi): (u32, u32);
            core::arch::asm!("rdmsr", in("ecx") 0x982u32, out("eax") lo, out("edx") hi, options(nostack));
            let activate = (hi as u64) << 32 | lo as u64;
            serial::write_str("[MEM] firmware TME: ");
            serial::write_str(if activate & 2 == 0 { "disabled" }
                else if activate & (1 << 31) != 0 { "encryption bypassed" }
                else { "enabled (firmware exclusions may apply)" });
            serial::write_str(if activate & 1 != 0 { ", locked\n" } else { ", unlocked\n" });
        } else { serial::write_line("[MEM] firmware TME: unavailable"); }
        if __cpuid(0x8000_0000).eax >= 0x8000_001f && __cpuid(0x8000_001f).eax & 1 != 0 {
            serial::write_line("[MEM] SME supported; kernel C-bit mapping integration not enabled");
        }
    }
}
