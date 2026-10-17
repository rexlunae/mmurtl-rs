//! Architectural fields and firmware availability, without assuming that
//! an advertised workaround has already been enabled.
use core::arch::asm;
use core::sync::atomic::{AtomicU8, Ordering};
use crate::security::{SpecCaps, Vulnerability as V, feature as F};
use super::fdt::PsciConduit;
static CONDUIT: AtomicU8 = AtomicU8::new(0);
pub fn set_conduit(conduit: PsciConduit) {
    CONDUIT.store(match conduit { PsciConduit::None => 0, PsciConduit::Hvc => 1, PsciConduit::Smc => 2 }, Ordering::Release);
}
pub fn smccc(function: u64, argument: u64) -> i64 {
    let mut result = function;
    unsafe {
        match CONDUIT.load(Ordering::Acquire) {
            1 => asm!("hvc #0", inout("x0") result, inlateout("x1") argument => _,
                inlateout("x2") 0u64 => _, inlateout("x3") 0u64 => _, clobber_abi("C")),
            2 => asm!("smc #0", inout("x0") result, inlateout("x1") argument => _,
                inlateout("x2") 0u64 => _, inlateout("x3") 0u64 => _, clobber_abi("C")),
            _ => return -1,
        }
    }
    // Architecture service functions use SMC32/HVC32: upper bits are not a sign extension.
    crate::security::smccc_status(result)
}
pub fn enumerate(cpu: usize) {
    super::mmu::record_asid_bits(cpu);
    let mut caps = SpecCaps::default();
    unsafe {
        asm!("mrs {}, midr_el1", out(reg) caps.identity, options(nomem, nostack));
        asm!("mrs {}, id_aa64pfr0_el1", out(reg) caps.raw[0], options(nomem, nostack));
        asm!("mrs {}, id_aa64pfr1_el1", out(reg) caps.raw[1], options(nomem, nostack));
        asm!("mrs {}, id_aa64mmfr1_el1", out(reg) caps.raw[2], options(nomem, nostack));
        asm!("mrs {}, id_aa64isar2_el1", out(reg) caps.raw[3], options(nomem, nostack));
        asm!("mrs {}, id_aa64mmfr2_el1", out(reg) caps.raw[4], options(nomem, nostack));
    }
    caps.vendor = *b"ARM         ";
    let csv2 = (caps.raw[0] >> 56) & 15;
    let csv3 = (caps.raw[0] >> 60) & 15;
    if matches!(csv2, 1..=3) { caps.not_affected |= V::SpectreV2.bit(); }
    if csv2 == 3 { caps.not_affected |= V::Bhb.bit(); }
    if csv3 == 1 { caps.not_affected |= V::Meltdown.bit(); }
    if (caps.raw[0] >> 48) & 15 == 1 { caps.features |= F::DIT; }
    if matches!((caps.raw[1] >> 4) & 15, 1 | 2) { caps.features |= F::SSBS; }
    if (caps.raw[1] >> 4) & 15 == 2 { caps.features |= F::SSBS2; }
    if (caps.raw[2] >> 60) & 15 == 1 { caps.features |= F::ECBHB; }
    if (caps.raw[3] >> 28) & 15 == 1 { caps.features |= F::CLRBHB; }
    caps.features |= crate::security::arm_pan::features(caps.raw[2], caps.raw[4]);
    // Core tables are deliberately small and conservative. Unknown cores
    // remain unknown unless architectural immunity is advertised.
    if caps.identity >> 24 & 255 == 0x41 {
        let part = (caps.identity >> 4) & 0xfff;
        if matches!(part, 0xd03 | 0xd04 | 0xd05) { // Cortex-A53/A35/A55
            caps.not_affected |= V::SpectreV2.bit() | V::Bhb.bit() | V::Ssb.bit();
        }
        if matches!(part, 0xd03 | 0xd04 | 0xd05 | 0xd07 | 0xd08 | 0xd09 | 0xd0b) {
            caps.not_affected |= V::Meltdown.bit(); // known unaffected cores; A75 excluded
        }
    }
    let version = smccc(0x8000_0000, 0);
    caps.raw[8] = version as u64;
    for (index, function, feature) in [(4,0x8000_8000,F::WA1),(5,0x8000_7fff,F::WA2),(6,0x8000_3fff,F::WA3)] {
        let status = if version >= 0x10001 { smccc(0x8000_0001, function) } else { -1 };
        caps.raw[index] = status as u64;
        if status == 0 { caps.features |= feature; }
        if status == 1 || index == 5 && status == -2 {
            caps.not_affected |= match index { 4 => V::SpectreV2, 5 => V::Ssb, _ => V::Bhb }.bit();
        }
    }
    crate::security::record(cpu, caps);
}

pub fn applicable(family: V) -> bool {
    matches!(family, V::SpectreV1 | V::SpectreV2 | V::Meltdown | V::Ssb | V::Bhb)
}

/// Bounded SMCCC v1.1 TRNG_RND64 discovery and requests (DEN0098).
pub fn trng_seed() -> Option<[u8; 48]> {
    if smccc(0x8000_0000, 0) < 0x0001_0001 || smccc(0x8400_0050, 0) < 0
        || smccc(0x8400_0051, 0xc400_0053) < 0 { return None; }
    let mut bytes = [0u8; 48];
    for chunk in bytes.chunks_mut(24) {
        let mut success = false;
        for _ in 0..16 {
            let mut x0 = 0xc400_0053u64;
            let mut x1 = 192u64; let x2: u64; let x3: u64;
            unsafe {
                match CONDUIT.load(Ordering::Acquire) {
                    1 => asm!("hvc #0", inout("x0") x0, inout("x1") x1, lateout("x2") x2, lateout("x3") x3, clobber_abi("C")),
                    2 => asm!("smc #0", inout("x0") x0, inout("x1") x1, lateout("x2") x2, lateout("x3") x3, clobber_abi("C")),
                    _ => return None,
                }
            }
            if x0 == 0 {
                chunk[..8].copy_from_slice(&x3.to_le_bytes());
                chunk[8..16].copy_from_slice(&x2.to_le_bytes());
                chunk[16..24].copy_from_slice(&x1.to_le_bytes());
                success = true; break;
            }
            if crate::security::smccc_status(x0) != -3 { break; } // only NO_ENTROPY is retryable
        }
        if !success { crate::random::crypto::erase(&mut bytes); return None; }
    }
    Some(bytes)
}
