//! RNDR is optional; unsupported systems keep uncredited timing input.
pub fn counter() -> u64 {
    let value: u64; unsafe { core::arch::asm!("mrs {}, cntvct_el0", out(reg) value, options(nomem, nostack)); } value
}
pub fn collect() { collect_inner(false); }
pub fn early_collect() { collect_inner(true); }
fn collect_inner(quiet: bool) {
    let isar: u64;
    unsafe { core::arch::asm!("mrs {}, id_aa64isar0_el1", out(reg) isar, options(nomem, nostack)); }
    if (isar >> 60) & 15 == 1 {
        for reseed in [true, false] {
            let mut bytes = [0u8; 32];
            let mut complete = true;
            for chunk in bytes.chunks_mut(8) {
                let mut got = false;
                for _ in 0..64 {
                    let value: u64; let valid: u64;
                    unsafe {
                        if reseed { core::arch::asm!("mrs {}, S3_3_C2_C4_1", "cset {}, ne", out(reg) value, out(reg) valid, options(nomem, nostack)); }
                        else { core::arch::asm!("mrs {}, S3_3_C2_C4_0", "cset {}, ne", out(reg) value, out(reg) valid, options(nomem, nostack)); }
                    }
                    if valid != 0 { chunk.copy_from_slice(&value.to_le_bytes()); got = true; break; }
                    core::hint::spin_loop();
                }
                complete &= got;
            }
            let name = if reseed { "RNDRRS" } else { "RNDR" };
            let credit = if complete { 256 } else { 0 };
            if quiet { crate::random::seed_quiet(name, &mut bytes, credit); }
            else { crate::random::seed(name, &mut bytes, credit); }
        }
    }
    if let Some(mut bytes) = super::cpu_features::trng_seed() {
        if quiet { crate::random::seed_quiet("SMCCC TRNG", &mut bytes, 256); }
        else { crate::random::seed("SMCCC TRNG", &mut bytes, 256); }
    }
}
