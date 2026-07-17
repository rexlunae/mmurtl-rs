//! ACPI table parsing — just enough to bring up SMP.
//!
//! We walk RSDP → RSDT/XSDT → MADT ("APIC" table) to discover:
//!   - the Local APIC MMIO base address
//!   - every processor's Local APIC ID (for INIT-SIPI-SIPI)
//!   - the I/O APIC base + GSI base (for routing legacy IRQs)
//!   - interrupt source overrides (ISA IRQ → GSI remaps)
//!
//! All tables are read through the physical-memory offset mapping; pages
//! that the bootloader didn't map are mapped on demand.

use alloc::vec::Vec;
use spin::Once;

/// A single ISA IRQ → GSI override from the MADT
#[derive(Debug, Clone, Copy)]
pub struct IrqOverride {
    pub source_irq: u8,
    pub gsi: u32,
    /// MPS INTI flags: bits 0-1 polarity, bits 2-3 trigger mode
    pub flags: u16,
}

/// Everything we learned from the MADT
pub struct MadtInfo {
    /// Physical address of the Local APIC MMIO window
    pub lapic_base: u64,
    /// Local APIC IDs of all enabled processors (BSP included)
    pub cpu_apic_ids: Vec<u32>,
    /// Physical address of the first I/O APIC (if any)
    pub ioapic_base: Option<u64>,
    /// GSI base of that I/O APIC
    pub ioapic_gsi_base: u32,
    /// ISA IRQ redirection overrides
    pub overrides: Vec<IrqOverride>,
}

impl MadtInfo {
    /// Resolve an ISA IRQ to its GSI + flags, honoring overrides
    pub fn isa_irq_to_gsi(&self, irq: u8) -> (u32, u16) {
        for ov in self.overrides.iter() {
            if ov.source_irq == irq {
                return (ov.gsi, ov.flags);
            }
        }
        (irq as u32, 0) // identity-mapped, conforming polarity/trigger
    }
}

static MADT: Once<MadtInfo> = Once::new();

/// Get the parsed MADT info (None if ACPI parsing failed)
pub fn madt() -> Option<&'static MadtInfo> {
    MADT.get()
}

// ========================================================================
// Raw table structures
// ========================================================================

/// Common ACPI System Description Table header (36 bytes)
#[repr(C, packed)]
struct SdtHeader {
    signature: [u8; 4],
    length: u32,
    revision: u8,
    checksum: u8,
    oem_id: [u8; 6],
    oem_table_id: [u8; 8],
    oem_revision: u32,
    creator_id: u32,
    creator_revision: u32,
}

const SDT_HEADER_LEN: usize = core::mem::size_of::<SdtHeader>();

/// Access a physical address as a virtual pointer, mapping it if needed
unsafe fn phys_ptr(phys: u64, len: u64) -> *const u8 {
    crate::memory::ensure_phys_mapped(phys, len, false);
    crate::memory::page_table::phys_to_virt(x86_64::PhysAddr::new(phys)).as_ptr()
}

/// Read an SDT header at a physical address, returning (header ptr, table length)
unsafe fn sdt_at(phys: u64) -> (*const u8, usize) {
    let ptr = phys_ptr(phys, SDT_HEADER_LEN as u64);
    let hdr = &*(ptr as *const SdtHeader);
    let len = hdr.length as usize;
    // Re-ensure the full table is mapped (may span extra pages)
    let ptr = phys_ptr(phys, len as u64);
    (ptr, len)
}

fn sig_matches(ptr: *const u8, sig: &[u8; 4]) -> bool {
    unsafe { core::slice::from_raw_parts(ptr, 4) == sig }
}

// ========================================================================
// Parsing
// ========================================================================

/// Parse ACPI tables starting from the RSDP physical address.
///
/// Returns true if a MADT was found and parsed.
pub fn init(rsdp_addr: Option<u64>) -> bool {
    let rsdp_phys = match rsdp_addr {
        Some(addr) => addr,
        None => {
            crate::serial::write_line("[ACPI] No RSDP provided by bootloader");
            return false;
        }
    };

    crate::serial::write_str("[ACPI] RSDP at 0x");
    crate::serial::write_hex(rsdp_phys);
    crate::serial::write_str("\n");

    unsafe {
        let rsdp = phys_ptr(rsdp_phys, 36);
        if core::slice::from_raw_parts(rsdp, 8) != b"RSD PTR " {
            crate::serial::write_line("[ACPI] Bad RSDP signature");
            return false;
        }

        let revision = *rsdp.add(15);
        let rsdt_addr = (rsdp.add(16) as *const u32).read_unaligned() as u64;
        let xsdt_addr = if revision >= 2 {
            (rsdp.add(24) as *const u64).read_unaligned()
        } else {
            0
        };

        // Prefer XSDT (64-bit entries) when available
        let madt_phys = if xsdt_addr != 0 {
            find_table(xsdt_addr, 8, b"APIC")
        } else {
            find_table(rsdt_addr, 4, b"APIC")
        };

        let madt_phys = match madt_phys {
            Some(addr) => addr,
            None => {
                crate::serial::write_line("[ACPI] MADT not found");
                return false;
            }
        };

        parse_madt(madt_phys)
    }
}

/// Scan an RSDT (entry_size=4) or XSDT (entry_size=8) for a table signature
unsafe fn find_table(sdt_phys: u64, entry_size: usize, sig: &[u8; 4]) -> Option<u64> {
    let (ptr, len) = sdt_at(sdt_phys);
    if len < SDT_HEADER_LEN {
        return None;
    }
    let entries = (len - SDT_HEADER_LEN) / entry_size;
    for i in 0..entries {
        let entry_ptr = ptr.add(SDT_HEADER_LEN + i * entry_size);
        let table_phys = if entry_size == 8 {
            (entry_ptr as *const u64).read_unaligned()
        } else {
            (entry_ptr as *const u32).read_unaligned() as u64
        };
        if table_phys == 0 {
            continue;
        }
        let (table_ptr, _) = sdt_at(table_phys);
        if sig_matches(table_ptr, sig) {
            return Some(table_phys);
        }
    }
    None
}

/// Parse the MADT: local APIC base, CPU list, I/O APIC, IRQ overrides
unsafe fn parse_madt(madt_phys: u64) -> bool {
    let (ptr, len) = sdt_at(madt_phys);

    let mut info = MadtInfo {
        lapic_base: (ptr.add(36) as *const u32).read_unaligned() as u64,
        cpu_apic_ids: Vec::new(),
        ioapic_base: None,
        ioapic_gsi_base: 0,
        overrides: Vec::new(),
    };

    // Entries start after header (36) + local APIC addr (4) + flags (4)
    let mut off = 44usize;
    while off + 2 <= len {
        let entry_type = *ptr.add(off);
        let entry_len = *ptr.add(off + 1) as usize;
        if entry_len < 2 || off + entry_len > len {
            break;
        }
        let e = ptr.add(off);

        match entry_type {
            // Processor Local APIC
            0 => {
                let apic_id = *e.add(3) as u32;
                let flags = (e.add(4) as *const u32).read_unaligned();
                // Bit 0: enabled, bit 1: online-capable
                if flags & 0b11 != 0 {
                    info.cpu_apic_ids.push(apic_id);
                }
            }
            // I/O APIC — keep the first one (GSI base 0 covers legacy IRQs)
            1 => {
                let addr = (e.add(4) as *const u32).read_unaligned() as u64;
                let gsi_base = (e.add(8) as *const u32).read_unaligned();
                if info.ioapic_base.is_none() {
                    info.ioapic_base = Some(addr);
                    info.ioapic_gsi_base = gsi_base;
                }
            }
            // Interrupt Source Override
            2 => {
                info.overrides.push(IrqOverride {
                    source_irq: *e.add(3),
                    gsi: (e.add(4) as *const u32).read_unaligned(),
                    flags: (e.add(8) as *const u16).read_unaligned(),
                });
            }
            // Local APIC Address Override (64-bit base)
            5 => {
                info.lapic_base = (e.add(4) as *const u64).read_unaligned();
            }
            // Processor Local x2APIC
            9 => {
                let apic_id = (e.add(4) as *const u32).read_unaligned();
                let flags = (e.add(8) as *const u32).read_unaligned();
                if flags & 0b11 != 0 {
                    info.cpu_apic_ids.push(apic_id);
                }
            }
            _ => {}
        }
        off += entry_len;
    }

    crate::serial::write_str("[ACPI] MADT: LAPIC base 0x");
    crate::serial::write_hex(info.lapic_base);
    crate::serial::write_str(", ");
    crate::serial::write_dec(info.cpu_apic_ids.len() as u64);
    crate::serial::write_str(" CPU(s)");
    if let Some(io) = info.ioapic_base {
        crate::serial::write_str(", IOAPIC at 0x");
        crate::serial::write_hex(io);
    }
    crate::serial::write_str(", ");
    crate::serial::write_dec(info.overrides.len() as u64);
    crate::serial::write_str(" IRQ override(s)\n");

    let found = !info.cpu_apic_ids.is_empty();
    MADT.call_once(|| info);
    found
}
