//! Boot-only CPU microcode loading. Module and patch buffers stay kernel-owned.
use spin::Mutex;
use super::microcode_msr as msr;
use crate::microcode::{self as format, Patch};
struct State { module: Option<&'static [u8]>, closed: bool }
static STATE: Mutex<State> = Mutex::new(State { module: None, closed: false });
const MAX_MODULE: u64 = 64 * 1024 * 1024;

pub fn init(address: Option<u64>, len: u64) {
    assert!(!x86_64::instructions::interrupts::are_enabled());
    let mut state = STATE.lock();
    assert!(!state.closed && state.module.is_none());
    if let Some(address) = address.filter(|a| *a != 0) {
        if len != 0 && len <= MAX_MODULE && address.checked_add(len).is_some() {
            state.module = Some(unsafe { core::slice::from_raw_parts(address as *const u8, len as usize) });
        } else { crate::serial::write_line("[UCODE] Invalid/oversized boot module ignored"); }
    }
}
fn vendor() -> (bool, bool, u32) {
    use core::arch::x86_64::__cpuid;
    let v = unsafe { __cpuid(0) };
    let mut name = [0; 12];
    name[..4].copy_from_slice(&v.ebx.to_le_bytes()); name[4..8].copy_from_slice(&v.edx.to_le_bytes()); name[8..].copy_from_slice(&v.ecx.to_le_bytes());
    let cpu = unsafe { __cpuid(1).eax };
    (name == *b"GenuineIntel", name == *b"AuthenticAMD", cpu)
}
fn revision(intel: bool) -> Option<u32> {
    if intel {
        if !msr::write(0x8b, 0) { return None; }
        unsafe { core::arch::x86_64::__cpuid(1); }
        Some((msr::read(0x8b)? >> 32) as u32)
    } else { Some(msr::read(0x8b)? as u32) }
}
fn trusted_amd(patch: Patch<'_>) -> bool {
    use sha2::{Digest, Sha256};
    let digest: [u8; 32] = Sha256::digest(patch.bytes).into();
    format::amd_digest_known(patch.revision, patch.bytes.len(), &digest)
}

/// Returns false for a late AP: it must park, without updating any CPU.
pub fn apply(cpu: usize) -> bool {
    assert!(!x86_64::instructions::interrupts::are_enabled());
    let state = STATE.lock(); // serialize BSP/AP updates and the boot seal
    if state.closed { return false; }
    msr::with_ap_fixups(cpu, || apply_locked(cpu, &state))
}
fn apply_locked(cpu: usize, state: &State) -> bool {
    msr::self_test();
    if unsafe { core::arch::x86_64::__cpuid(1).ecx & (1 << 31) != 0 } {
        crate::serial::write_str("[UCODE] CPU"); crate::serial::write_dec(cpu as u64);
        crate::serial::write_line(" hypervisor: host owns microcode (no update MSRs)"); return true;
    }
    let (intel, amd, signature) = vendor();
    let fam = format::family(signature);
    if (!intel || !matches!(fam, 6 | 15)) && (!amd || fam < 0x10) {
        crate::serial::write_str("[UCODE] CPU"); crate::serial::write_dec(cpu as u64);
        crate::serial::write_line(" unsupported vendor/family; firmware microcode retained"); return true;
    }
    let Some(before) = revision(intel) else { crate::serial::write_line("[UCODE] revision MSR unavailable; update skipped"); return true; };
    if amd && fam >= 0x17 && before == 0 {
        crate::serial::write_line("[UCODE] Unknown AMD signing baseline; update BIOS before OS microcode");
        return true;
    }
    let patch = state.module.map(|blob| {
        if intel {
            let platform = match msr::read(0x17) { Some(v) => 1u32 << ((v >> 50) & 7), None => return Ok(None) };
            format::intel(blob, signature, platform, before)
        } else { format::amd(blob, signature, before) }
    });
    let selected = match patch {
        Some(Ok(Some(p))) if !amd || (trusted_amd(p) && format::amd_policy::transition_allowed(before, p.revision)) => Some(p),
        Some(Ok(Some(_))) => { crate::serial::write_line("[UCODE] AMD digest/signing transition is not trusted; update refused"); None },
        Some(Err(_)) => { crate::serial::write_line("[UCODE] Malformed vendor container; update refused"); None },
        _ => None,
    };
    if let Some(patch) = selected {
        let pages = (patch.bytes.len() + 4095) / 4096;
        let allocation = crate::memory::heap::with_frame_allocator(|fa| fa.allocate_zeroed_contiguous(pages)).flatten();
        if let Some(pa) = allocation {
            let pointer = crate::arch::phys_to_virt(pa);
            unsafe {
                core::ptr::copy_nonoverlapping(patch.bytes.as_ptr(), pointer, patch.bytes.len());
                // Both vendor interfaces take a linear address. Pages provide
                // WB memory and at least the required 16-byte alignment.
                let address = pointer.add(patch.data_offset) as u64;
                if !msr::write(if intel { 0x79 } else { 0xc001_0020 }, address) {
                    crate::serial::write_line("[UCODE] update MSR refused; firmware revision retained");
                }
                core::arch::x86_64::__cpuid(1);
                if amd && fam == 0x17 {
                    for offset in (0..patch.bytes.len()).step_by(4096) {
                        x86_64::instructions::tlb::flush(x86_64::VirtAddr::new(pointer.add(offset) as u64));
                    }
                }
            }
            let after = revision(intel).unwrap_or(before);
            if after != patch.revision { crate::serial::write_line("[UCODE] Update revision differs from selected patch; reporting actual CPU revision"); }
            crate::memory::heap::with_frame_allocator(|fa| {
                for i in 0..pages { fa.deallocate_frame(pa + (i * 4096) as u64); }
            });
        } else { crate::serial::write_line("[UCODE] No update buffer; firmware revision retained"); }
    }
    let after = revision(intel).unwrap_or(before);
    crate::serial::write_str("[UCODE] CPU"); crate::serial::write_dec(cpu as u64);
    crate::serial::write_str(" rev 0x"); crate::serial::write_hex(before as u64);
    crate::serial::write_str(" -> 0x"); crate::serial::write_hex(after as u64); crate::serial::write_str("\n");
    if amd && signature == 0x0083_0f10 && after < 0x0830_107b {
        // AMD-SB-7008: EPYC Rome Zenbleed minimum, vendor advisory.
        crate::serial::write_line("[UCODE] WARNING: below AMD-SB-7008 Zenbleed minimum 0x0830107b");
    }
    if state.module.is_none() { crate::serial::write_line("[UCODE] No boot module: firmware revision retained; security fixes may be missing"); }
    true
}

/// Seal while all previously started APs are still spinning, before scheduling.
pub fn seal() { STATE.lock().closed = true; }
