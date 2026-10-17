//! Local topology; never infer another CPU's SMT width from the BSP.
use crate::topology::policy::{Core, Source};
use core::arch::x86_64::{__cpuid, __cpuid_count};
pub fn probe() -> Core {
    unsafe {
        let max = __cpuid(0).eax;
        let one = __cpuid(1);
        let legacy_id = one.ebx >> 24;
        for (leaf, source) in [(0x1f, Source::Intel1f), (0xb, Source::IntelB)] {
            if max < leaf { continue; }
            let mut smt = None; let mut package_shift = 0; let mut id = None; let mut invalid = false;
            for level in 0..8 {
                let r = __cpuid_count(leaf, level);
                if r.ebx & 65535 == 0 { break; }
                if id.is_some_and(|old| old != r.edx) { invalid = true; break; }
                id = Some(r.edx);
                let shift = r.eax & 31;
                package_shift = package_shift.max(shift);
                if (r.ecx >> 8) & 255 == 1 { smt = Some((shift, r.ebx & 65535)); }
            }
            if !invalid {
                if let (Some(id), Some((shift, threads))) = (id, smt) {
                    if let Some(core) = Core::x86(id, shift, package_shift, threads, source) { return core; }
                }
            }
        }
        let ext = __cpuid(0x80000000).eax;
        let topoext = ext >= 0x8000001e && __cpuid(0x80000001).ecx & (1 << 22) != 0;
        if topoext {
            let r = __cpuid(0x8000001e);
            let threads = ((r.ebx >> 8) & 255) + 1;
            if threads.is_power_of_two() {
                let shift = threads.trailing_zeros();
                // Extended APIC ID supplies the full globally unique core key.
                if let Some(core) = Core::x86(r.eax, shift, { let ecx = __cpuid(0x80000008).ecx; let bits = (ecx >> 12) & 15; if bits != 0 { bits } else { 32 - (ecx & 255).leading_zeros() } }, threads, Source::Amd1e) { return core; }
            }
        }
        if one.edx & (1 << 28) == 0 { return Core::x86(legacy_id,0,0,1,Source::Legacy).unwrap(); }
        let logical = (one.ebx >> 16) & 255;
        let cache = if max >= 4 { __cpuid_count(4,0) } else { __cpuid_count(0,0) };
        if max >= 4 && cache.eax & 31 != 0 && (cache.eax >> 5) & 7 == 1 {
            let threads = ((cache.eax >> 14) & 0xfff) + 1;
            if threads.is_power_of_two() && logical != 0 {
                let package_shift = 32 - (logical - 1).leading_zeros();
                if let Some(core) = Core::x86(legacy_id, threads.trailing_zeros(), package_shift, threads, Source::Legacy) { return core; }
            }
        }
        let mut cores = None;
        // Old AMD NC counts physical cores; on Zen it may count threads.
        // Modern AMD must use leaf B/1F or TopologyExtensions above.
        let family = ((one.eax >> 8) & 15) + if (one.eax >> 8) & 15 == 15 { (one.eax >> 20) & 255 } else { 0 };
        let vendor = __cpuid(0);
        if cores.is_none() && vendor.ebx == 0x68747541 && family < 0x17 && ext >= 0x80000008 {
            cores = Some((__cpuid(0x80000008).ecx & 255) + 1);
        }
        if let Some(cores) = cores {
            if logical != 0 && logical >= cores && logical % cores == 0 {
                let threads = logical / cores;
                if threads.is_power_of_two() {
                    let package_shift = 32 - (logical - 1).leading_zeros();
                    if let Some(core) = Core::x86(legacy_id,threads.trailing_zeros(),package_shift,threads,Source::Legacy) { return core; }
                }
            }
        }
        Core::unknown(legacy_id as u64)
    }
}
