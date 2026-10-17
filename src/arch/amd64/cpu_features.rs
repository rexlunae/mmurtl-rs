//! CPUID-gated reads only: capability does not imply a mitigation is active.
use crate::security::{SpecCaps, Vulnerability as V, feature as F};
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
        for (bit, feature) in [(26,F::IBRS | F::IBPB),(27,F::STIBP),(28,F::L1D_FLUSH),
            (29,F::ARCH_CAPS),(31,F::SSBD),(10,F::MD_CLEAR)] {
            if leaf.edx & (1 << bit) != 0 { caps.features |= feature; }
        }
        if leaf.ebx & (1 << 7) != 0 { caps.features |= F::SMEP; }
        if leaf.ebx & (1 << 20) != 0 { caps.features |= F::SMAP; }
        if leaf.ecx & (1 << 2) != 0 { caps.features |= F::UMIP; }
        if leaf.eax >= 1 {
            caps.raw[7] = __cpuid_count(7, 1).eax as u64;
            if caps.raw[7] & (1 << 6) != 0 { caps.features |= F::LASS; }
        }
        if leaf.eax >= 2 {
            caps.raw[1] = __cpuid_count(7, 2).edx as u64;
            for (bit, feature) in [(0,F::PSFD),(1,F::IPRED_CTRL),(2,F::RRSBA_CTRL),(4,F::BHI_CTRL)] {
                if caps.raw[1] & (1 << bit) != 0 { caps.features |= feature; }
            }
        }
        if caps.features & F::ARCH_CAPS != 0 {
            let (lo, hi): (u32, u32);
            unsafe { core::arch::asm!("rdmsr", in("ecx") 0x10au32, out("eax") lo, out("edx") hi, options(nostack)); }
            caps.raw[4] = (hi as u64) << 32 | lo as u64;
            for (bit, family) in [(0,V::Meltdown),(4,V::Ssb),(5,V::Mds),(8,V::Taa),
                (20,V::Bhi),(26,V::Gds),(27,V::Rfds)] {
                if caps.raw[4] & (1 << bit) != 0 { caps.not_affected |= family.bit(); }
            }
            if caps.raw[4] & ((1 << 13)|(1 << 14)|(1 << 15)) == ((1 << 13)|(1 << 14)|(1 << 15)) {
                caps.not_affected |= V::Mmio.bit();
            }
            for (bit, feature) in [(1,F::EIBRS),(12,F::DOITM),(25,F::GDS_CTRL),(28,F::RFDS_CLEAR)] {
                if caps.raw[4] & (1 << bit) != 0 { caps.features |= feature; }
            }
        }
    }
    let max_ext = __cpuid(0x8000_0000).eax;
    if caps.vendor == *b"AuthenticAMD" {
        if max_ext >= 0x8000_0008 {
            caps.raw[2] = __cpuid(0x8000_0008).ebx as u64;
            for (bit, feature) in [(12,F::IBPB),(14,F::IBRS),(15,F::STIBP),
                (24,F::SSBD),(25,F::VIRT_SSBD),(30,F::IBPB_RET)] {
                if caps.raw[2] & (1 << bit) != 0 { caps.features |= feature; }
            }
            if caps.raw[2] & (1 << 26) != 0 { caps.not_affected |= V::Ssb.bit(); }
        }
        if max_ext >= 0x8000_0021 {
            caps.raw[3] = __cpuid(0x8000_0021).eax as u64;
            if caps.raw[3] & (1 << 8) != 0 { caps.features |= F::AUTOIBRS; }
            if caps.raw[3] & (1 << 27) != 0 { caps.features |= F::SBPB; }
            if caps.raw[3] & (1 << 29) != 0 { caps.not_affected |= V::Srso.bit(); }
        }
    }
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
