//! Legacy (0.9.5) virtio-pci transport over PCI I/O space — shared by
//! both ports.
//!
//! QEMU's transitional virtio-pci devices (vendor 0x1AF4, device IDs
//! 0x1000-0x103F) expose the legacy interface through an I/O BAR, which is
//! far simpler to drive than the modern capability-based interface: a
//! fixed register layout, guest-endian (little-endian on both our
//! architectures), and page-frame-number queue addressing. I/O space is
//! reached through `crate::arch::io_*`: port instructions on amd64, the
//! host bridge's memory-mapped I/O window on arm64.

use alloc::boxed::Box;

/// Minimal port-I/O helper over the architecture's I/O space
struct Port<T>(u16, core::marker::PhantomData<T>);

impl<T> Port<T> {
    fn new(port: u16) -> Self {
        Self(port, core::marker::PhantomData)
    }
}
impl Port<u8> {
    fn read(&mut self) -> u8 {
        crate::arch::io_read8(self.0)
    }
    fn write(&mut self, v: u8) {
        crate::arch::io_write8(self.0, v)
    }
}
impl Port<u16> {
    fn read(&mut self) -> u16 {
        crate::arch::io_read16(self.0)
    }
    fn write(&mut self, v: u16) {
        crate::arch::io_write16(self.0, v)
    }
}
impl Port<u32> {
    fn read(&mut self) -> u32 {
        crate::arch::io_read32(self.0)
    }
    fn write(&mut self, v: u32) {
        crate::arch::io_write32(self.0, v)
    }
}

use crate::virtio::{Transport, Virtqueue, STATUS_ACKNOWLEDGE, STATUS_DRIVER, STATUS_DRIVER_OK};

pub const VIRTIO_VENDOR_ID: u16 = 0x1AF4;

// Legacy I/O register offsets (no MSI-X)
const REG_HOST_FEATURES: u16 = 0x00; // r32
const REG_GUEST_FEATURES: u16 = 0x04; // w32
const REG_QUEUE_PFN: u16 = 0x08; // rw32
const REG_QUEUE_NUM: u16 = 0x0C; // r16
const REG_QUEUE_SEL: u16 = 0x0E; // w16
const REG_QUEUE_NOTIFY: u16 = 0x10; // w16
const REG_STATUS: u16 = 0x12; // rw8
const REG_ISR: u16 = 0x13; // r8 (read acknowledges)
/// Device-specific config starts here (without MSI-X)
const REG_DEVICE_CONFIG: u16 = 0x14;

// ========================================================================
// Legacy virtio-pci transport
// ========================================================================

pub struct VirtioLegacy {
    io: u16,
}

impl VirtioLegacy {
    /// Take ownership of a transitional virtio-pci device: enable bus
    /// mastering, reset it, and acknowledge it. Returns None if BAR0 is
    /// not an I/O BAR (modern-only device).
    pub fn new(dev: &crate::pci::PciDevice) -> Option<Self> {
        let io = dev.io_base()?;
        dev.enable_bus_master();

        let t = Self { io };
        t.write_status(0); // reset
        t.write_status(STATUS_ACKNOWLEDGE);
        t.write_status(STATUS_ACKNOWLEDGE | STATUS_DRIVER);
        Some(t)
    }

    fn write_status(&self, status: u8) {
        { Port::<u8>::new(self.io + REG_STATUS).write(status) }
    }

    pub fn host_features(&self) -> u32 {
        { Port::<u32>::new(self.io + REG_HOST_FEATURES).read() }
    }

    pub fn set_guest_features(&self, features: u32) {
        { Port::<u32>::new(self.io + REG_GUEST_FEATURES).write(features) }
    }

    /// Select a virtqueue and return its size (0 = queue doesn't exist)
    pub fn queue_size(&self, queue: u16) -> u16 {
        {
            Port::<u16>::new(self.io + REG_QUEUE_SEL).write(queue);
            Port::<u16>::new(self.io + REG_QUEUE_NUM).read()
        }
    }

    /// Register a virtqueue's ring memory
    pub fn set_queue_pfn(&self, queue: u16, phys: u64) {
        {
            Port::<u16>::new(self.io + REG_QUEUE_SEL).write(queue);
            Port::<u32>::new(self.io + REG_QUEUE_PFN).write((phys >> 12) as u32);
        }
    }

    /// Tell the device a queue has new buffers
    pub fn notify(&self, queue: u16) {
        { Port::<u16>::new(self.io + REG_QUEUE_NOTIFY).write(queue) }
    }

    /// Read + acknowledge the ISR status
    #[allow(dead_code)]
    pub fn isr(&self) -> u8 {
        { Port::<u8>::new(self.io + REG_ISR).read() }
    }

    /// Finish initialization — device is live after this
    pub fn driver_ok(&self) {
        self.write_status(STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_DRIVER_OK);
    }

    pub fn config_read8(&self, offset: u16) -> u8 {
        { Port::<u8>::new(self.io + REG_DEVICE_CONFIG + offset).read() }
    }

    pub fn config_read32(&self, offset: u16) -> u32 {
        { Port::<u32>::new(self.io + REG_DEVICE_CONFIG + offset).read() }
    }

    pub fn config_read64(&self, offset: u16) -> u64 {
        let lo = self.config_read32(offset) as u64;
        let hi = self.config_read32(offset + 4) as u64;
        (hi << 32) | lo
    }
}

impl Transport for VirtioLegacy {
    fn host_features(&self) -> u32 {
        VirtioLegacy::host_features(self)
    }
    fn set_guest_features(&self, features: u32) {
        VirtioLegacy::set_guest_features(self, features)
    }
    fn queue_max(&self, queue: u16) -> u16 {
        self.queue_size(queue)
    }
    fn setup_queue(&self, queue: u16, vq: &Virtqueue) {
        self.set_queue_pfn(queue, vq.ring_phys)
    }
    fn notify(&self, queue: u16) {
        VirtioLegacy::notify(self, queue)
    }
    fn driver_ok(&self) {
        VirtioLegacy::driver_ok(self)
    }
    fn config_read8(&self, offset: u16) -> u8 {
        VirtioLegacy::config_read8(self, offset)
    }
    fn config_read32(&self, offset: u16) -> u32 {
        VirtioLegacy::config_read32(self, offset)
    }
    fn name(&self) -> &'static str {
        "legacy virtio-pci"
    }
}

/// Probe the PCI bus for virtio devices and bring up the drivers we have
pub fn init(devices: &[crate::pci::PciDevice]) {
    for dev in devices {
        if dev.vendor_id != VIRTIO_VENDOR_ID {
            continue;
        }
        // Transitional device IDs: 0x1000 + (virtio device type - 1)
        let device_type = match dev.device_id {
            0x1000 => 1, // network
            0x1001 => 2, // block
            other => {
                crate::serial::write_str("[VIRTIO] Unhandled virtio-pci device 0x");
                crate::serial::write_hex(other as u64);
                crate::serial::write_str("
");
                continue;
            }
        };
        match VirtioLegacy::new(dev) {
            Some(t) => crate::virtio::attach(device_type, Box::new(t)),
            None => crate::serial::write_line("[VIRTIO] virtio-pci device has no I/O BAR (modern-only?) — skipped"),
        }
    }
}
