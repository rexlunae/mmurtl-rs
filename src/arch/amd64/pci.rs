//! PCI bus scanning — finds devices via port I/O config space (CF8/CFC).
//!
//! The PCI configuration space is accessed via I/O ports 0xCF8 and 0xCFC.
//! This module scans bus 0 for all devices and functions, and provides
//! helpers to find specific classes of devices (like USB controllers).

use x86_64::instructions::port::Port;

/// PCI config address register (CONFIG_ADDRESS)
#[derive(Debug)]
struct PciConfigAddress(u32);

impl PciConfigAddress {
    /// Build a PCI config address for a given bus/device/function/register
    fn new(bus: u8, device: u8, function: u8, register: u8) -> Self {
        // Bit 31: Enable bit
        // Bits 23-16: Bus number
        // Bits 15-11: Device number
        // Bits 10-8: Function number
        // Bits 7-2: Register index (dword aligned)
        Self(
            0x8000_0000u32
                | ((bus as u32) << 16)
                | ((device as u32) << 11)
                | ((function as u32) << 8)
                | (register as u32 & 0xFC),
        )
    }
}

/// Read a 32-bit value from PCI config space
fn pci_config_read(bus: u8, device: u8, function: u8, register: u8) -> u32 {
    unsafe {
        let mut address_port: Port<u32> = Port::new(0xCF8);
        let mut data_port: Port<u32> = Port::new(0xCFC);
        address_port.write(PciConfigAddress::new(bus, device, function, register).0);
        data_port.read()
    }
}

/// Write a 32-bit value to PCI config space
fn pci_config_write(bus: u8, device: u8, function: u8, register: u8, value: u32) {
    unsafe {
        let mut address_port: Port<u32> = Port::new(0xCF8);
        let mut data_port: Port<u32> = Port::new(0xCFC);
        address_port.write(PciConfigAddress::new(bus, device, function, register).0);
        data_port.write(value);
    }
}

/// PCI device identifiers
#[derive(Debug, Clone, Copy)]
pub struct PciDevice {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub vendor_id: u16,
    pub device_id: u16,
    pub class: u8,
    pub subclass: u8,
    pub prog_if: u8,
    pub revision: u8,
    pub bar0: u32,
    pub bar1: u32,
}

impl PciDevice {
    /// Read full config space header for device
    fn from_location(bus: u8, device: u8, function: u8) -> Self {
        let id_reg = pci_config_read(bus, device, function, 0x00);
        let class_reg = pci_config_read(bus, device, function, 0x08);
        let bar0_reg = pci_config_read(bus, device, function, 0x10);
        let bar1_reg = pci_config_read(bus, device, function, 0x14);

        Self {
            bus,
            device,
            function,
            vendor_id: (id_reg & 0xFFFF) as u16,
            device_id: ((id_reg >> 16) & 0xFFFF) as u16,
            class: ((class_reg >> 24) & 0xFF) as u8,
            subclass: ((class_reg >> 16) & 0xFF) as u8,
            prog_if: ((class_reg >> 8) & 0xFF) as u8,
            revision: (class_reg & 0xFF) as u8,
            bar0: bar0_reg,
            bar1: bar1_reg,
        }
    }

    /// Get the MMIO base address from BAR0 (memory-mapped BAR)
    pub fn mmio_base(&self) -> Option<u64> {
        if self.bar0 & 1 == 0 {
            // Memory BAR — lower bits indicate type, upper bits are base
            let base = self.bar0 & 0xFFFFFFF0;
            // Check if 64-bit BAR (BAR0 + BAR1 combined)
            if (self.bar0 & 0b110) == 0b100 {
                // 64-bit BAR: BAR0 = low 32 bits, BAR1 = high 32 bits
                Some((base as u64) | ((self.bar1 as u64) << 32))
            } else {
                Some(base as u64)
            }
        } else {
            None // I/O BAR
        }
    }

    /// Get the I/O port base from BAR0 (I/O BAR), if it is one
    pub fn io_base(&self) -> Option<u16> {
        if self.bar0 & 1 == 1 {
            Some((self.bar0 & 0xFFFC) as u16)
        } else {
            None
        }
    }

    /// Enable I/O space, memory space, and bus mastering in the command
    /// register — required before a device may DMA.
    pub fn enable_bus_master(&self) {
        let cmd_status = pci_config_read(self.bus, self.device, self.function, 0x04);
        let cmd = (cmd_status & 0xFFFF) | 0b111; // IO space | mem space | bus master
        pci_config_write(
            self.bus,
            self.device,
            self.function,
            0x04,
            (cmd_status & 0xFFFF_0000) | cmd,
        );
    }

    /// Human-readable class name
    pub fn class_name(&self) -> &'static str {
        match (self.class, self.subclass, self.prog_if) {
            (0x0C, 0x03, 0x30) => "xHCI (USB 3.x)",
            (0x0C, 0x03, 0x20) => "EHCI (USB 2.0)",
            (0x0C, 0x03, 0x10) => "OHCI (USB 1.x)",
            (0x0C, 0x03, 0x00) => "UHCI (USB 1.x)",
            (0x0C, 0x03, _)    => "USB Controller",
            (0x03, 0x00, _)    => "VGA Compatible",
            (0x01, 0x01, _)    => "IDE Controller",
            (0x01, 0x06, _)    => "SATA Controller",
            (0x02, 0x00, _)    => "Ethernet Controller",
            (0x06, 0x00, _)    => "Host Bridge",
            (0x06, 0x01, _)    => "ISA Bridge",
            (0x06, 0x04, _)    => "PCI-to-PCI Bridge",
            (0x04, 0x01, _)    => "Multimedia audio",
            _                  => "Other",
        }
    }
}

/// Scan PCI bus 0 and return all found devices
pub fn scan() -> heapless::Vec<PciDevice, 64> {
    let mut devices = heapless::Vec::new();

    for device in 0..32 {
        for function in 0..8 {
            let dev = PciDevice::from_location(0, device, function);
            if dev.vendor_id != 0xFFFF {
                // Valid device found
                let _ = devices.push(dev);
            }
        }
    }

    devices
}

/// Find all USB controllers on PCI bus 0
pub fn find_usb_controllers() -> heapless::Vec<PciDevice, 8> {
    let all = scan();
    let mut usb = heapless::Vec::new();

    for dev in all.iter() {
        if dev.class == 0x0C && dev.subclass == 0x03 {
            let _ = usb.push(*dev);
        }
    }

    usb
}
