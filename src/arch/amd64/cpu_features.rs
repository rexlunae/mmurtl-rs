//! CPUID-gated reads only: capability does not imply a mitigation is active.
use crate::security::{SpecCaps, Vulnerability as V};
pub fn enumerate(cpu: usize) {
    use core::arch::x86_64::{__cpuid, __cpuid_count};
    let base = __cpuid(0);
    let mut caps = SpecCaps::default();
    caps.vendor[..4].copy_from_slice(&base.ebx.to_le_bytes());
    caps.vendor[4..8].copy_from_slice(&base.edx.to_le_bytes());
    caps.vendor[8..].copy_from_slice(&base.ecx.to_le_bytes());
    caps.identity = __cpuid(1).eax as u64;
    if base.eax >= 7 {
        let leaf = __cpuid_count(7, 0);
        caps.raw[0] = leaf.edx as u64; caps.raw[5] = leaf.ebx as u64; caps.raw[6] = leaf.ecx as u64;
        if leaf.eax >= 1 { caps.raw[7] = __cpuid_count(7, 1).eax as u64; }
        if leaf.eax >= 2 { caps.raw[1] = __cpuid_count(7, 2).edx as u64; }
    }
    let max_ext = __cpuid(0x8000_0000).eax;
    if caps.vendor == *b"AuthenticAMD" {
        if max_ext >= 0x8000_0008 { caps.raw[2] = __cpuid(0x8000_0008).ebx as u64; }
        if max_ext >= 0x8000_0021 { caps.raw[3] = __cpuid(0x8000_0021).eax as u64; }
    }
    let caps = crate::security::x86_decode::decode(caps, || {
        let (lo, hi): (u32, u32);
        unsafe { core::arch::asm!("rdmsr", in("ecx") 0x10au32, out("eax") lo, out("edx") hi, options(nostack)); }
        (hi as u64) << 32 | lo as u64
    });
    crate::security::record(cpu, caps);
}

/// QEMU's read-only fw_cfg command-line equivalent. Physical hardware
/// can use the embedded MMURTL_BOOTARGS build configuration instead.
pub fn bootargs(buffer: &mut [u8; 512]) -> Option<usize> {
    use x86_64::instructions::port::Port;
    if core::arch::x86_64::__cpuid(1).ecx & (1 << 31) == 0 { return None; }
    unsafe {
        let mut select = Port::<u16>::new(0x510);
        let mut data = Port::<u8>::new(0x511);
        select.write(0);
        let mut signature = [0u8; 4];
        for byte in &mut signature { *byte = data.read(); }
        if signature != *b"QEMU" { return None; }
        select.write(0x19);
        let mut count = [0u8; 4];
        for byte in &mut count { *byte = data.read(); }
        let count = u32::from_be_bytes(count);
        if count > 1024 { return None; }
        for _ in 0..count {
            let mut entry = [0u8; 64];
            for byte in &mut entry { *byte = data.read(); }
            let name_end = entry[8..].iter().position(|b| *b == 0).unwrap_or(56);
            if &entry[8..8 + name_end] == b"opt/mmurtl/bootargs" {
                let size = u32::from_be_bytes(entry[..4].try_into().unwrap()) as usize;
                let key = u16::from_be_bytes(entry[4..6].try_into().unwrap());
                if size > buffer.len() || !(0x20..0x4000).contains(&key) { return None; }
                select.write(key);
                for byte in &mut buffer[..size] { *byte = data.read(); }
                return Some(size);
            }
        }
        None
    }
}

pub fn applicable(family: V) -> bool { family != V::Bhb }
