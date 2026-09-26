//! arm64 PCIe: a generic ECAM host bridge (QEMU `virt`'s gpex) from the
//! device tree.
//!
//! Configuration space is the memory-mapped ECAM window; PCI I/O space is
//! the host bridge's memory-mapped I/O window (port N lives at window base
//! + N). The kernel boots without firmware PCI setup, so it assigns BARs
//! itself (`crate::pci::assign_bars`) from the bridge's I/O and 32-bit
//! memory windows.

use core::sync::atomic::{AtomicU64, Ordering};

use super::fdt::PciHost;

static ECAM: AtomicU64 = AtomicU64::new(0);
static ECAM_BUSES: AtomicU64 = AtomicU64::new(0); // bus_start | bus_end << 8
static IO_BASE: AtomicU64 = AtomicU64::new(0);

/// Map the ECAM window, record the I/O window, and assign BARs
pub fn init(host: &PciHost) -> bool {
    if let Err(e) = super::mmu::map_device(host.ecam, host.ecam_size) {
        crate::serial::write_str("[PCI] Cannot map ECAM: ");
        crate::serial::write_line(e);
        return false;
    }
    ECAM.store(host.ecam, Ordering::Relaxed);
    ECAM_BUSES.store(host.bus_start as u64 | (host.bus_end as u64) << 8, Ordering::Relaxed);
    let (io_cpu, io_pci, io_size) = host.io;
    IO_BASE.store(io_cpu.wrapping_sub(io_pci), Ordering::Relaxed);

    let (mem_cpu, mem_pci, mem_size) = host.mem32;
    let mem_usable = mem_size != 0 && mem_cpu == mem_pci; // BAR value == CPU address
    let mut windows = crate::pci::BarWindows {
        // Keep port 0 and the legacy ISA range out of it
        io_next: io_pci + 0x1000,
        io_end: io_pci + io_size,
        mem_next: mem_pci,
        mem_end: if mem_usable { mem_pci + mem_size } else { mem_pci },
    };
    let bars = crate::pci::assign_bars(&mut windows);

    use core::fmt::Write;
    let mut line: heapless::String<160> = heapless::String::new();
    let _ = write!(
        line,
        "[PCI] ECAM host bridge at 0x{:x} (buses {}-{}); I/O window 0x{:x}, MMIO window 0x{:x}; {} BARs assigned\n",
        host.ecam, host.bus_start, host.bus_end, io_cpu, mem_cpu, bars
    );
    crate::serial::write_str(&line);
    true
}

fn cfg_addr(bus: u8, device: u8, function: u8, register: u8) -> Option<u64> {
    let ecam = ECAM.load(Ordering::Relaxed);
    let buses = ECAM_BUSES.load(Ordering::Relaxed);
    let (start, end) = (buses as u8, (buses >> 8) as u8);
    if ecam == 0 || bus < start || bus > end || device > 31 || function > 7 {
        return None;
    }
    Some(
        ecam + (((bus - start) as u64) << 20)
            + ((device as u64) << 15)
            + ((function as u64) << 12)
            + register as u64,
    )
}

/// PCI config read (all ones when there is no host bridge / no device)
pub fn pci_config_read(bus: u8, device: u8, function: u8, register: u8) -> u32 {
    match cfg_addr(bus, device, function, register) {
        Some(a) => unsafe { core::ptr::read_volatile(a as *const u32) },
        None => 0xFFFF_FFFF,
    }
}

pub fn pci_config_write(bus: u8, device: u8, function: u8, register: u8, value: u32) {
    if let Some(a) = cfg_addr(bus, device, function, register) {
        unsafe { core::ptr::write_volatile(a as *mut u32, value) }
    }
}

fn io_addr(port: u16) -> u64 {
    IO_BASE.load(Ordering::Relaxed) + port as u64
}

pub fn io_read8(port: u16) -> u8 {
    unsafe { core::ptr::read_volatile(io_addr(port) as *const u8) }
}
pub fn io_read16(port: u16) -> u16 {
    unsafe { core::ptr::read_volatile(io_addr(port) as *const u16) }
}
pub fn io_read32(port: u16) -> u32 {
    unsafe { core::ptr::read_volatile(io_addr(port) as *const u32) }
}
pub fn io_write8(port: u16, v: u8) {
    unsafe { core::ptr::write_volatile(io_addr(port) as *mut u8, v) }
}
pub fn io_write16(port: u16, v: u16) {
    unsafe { core::ptr::write_volatile(io_addr(port) as *mut u16, v) }
}
pub fn io_write32(port: u16, v: u32) {
    unsafe { core::ptr::write_volatile(io_addr(port) as *mut u32, v) }
}
