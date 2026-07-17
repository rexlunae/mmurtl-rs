//! Virtio — legacy (0.9.5) virtio-pci transport and split virtqueues.
//!
//! QEMU's transitional virtio-pci devices (vendor 0x1AF4, device IDs
//! 0x1000-0x103F) expose the legacy interface through an I/O port BAR,
//! which is far simpler to drive than the modern capability-based MMIO
//! interface: a fixed register layout, guest-endian (little-endian on
//! x86), and page-frame-number queue addressing.
//!
//! Drivers built on this: virtio-blk (storage) and virtio-net (network).

pub mod blk;
pub mod net;

use core::sync::atomic::{fence, Ordering};
use x86_64::instructions::port::Port;

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
pub const REG_DEVICE_CONFIG: u16 = 0x14;

// Device status bits
const STATUS_ACKNOWLEDGE: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;

// ========================================================================
// DMA memory
// ========================================================================

/// A physically contiguous, identity-translatable DMA region
#[derive(Clone, Copy)]
pub struct DmaRegion {
    pub phys: u64,
    pub virt: *mut u8,
    pub size: usize,
}

// The raw pointer targets leaked, globally mapped DMA memory; access is
// serialized by the owning driver's Mutex.
unsafe impl Send for DmaRegion {}

/// Allocate `pages` physically contiguous, zeroed pages for device DMA.
/// Accessed by the CPU through the physical-memory offset window.
pub fn dma_alloc(pages: usize) -> DmaRegion {
    let phys = crate::memory::heap::with_frame_allocator(|fa| fa.allocate_contiguous(pages))
        .expect("Frame allocator not initialized")
        .expect("OOM allocating DMA region");

    let virt = crate::memory::page_table::phys_to_virt(phys).as_mut_ptr::<u8>();
    let size = pages * 4096;
    unsafe { core::ptr::write_bytes(virt, 0, size) };

    DmaRegion {
        phys: phys.as_u64(),
        virt,
        size,
    }
}

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
        unsafe { Port::<u8>::new(self.io + REG_STATUS).write(status) }
    }

    pub fn host_features(&self) -> u32 {
        unsafe { Port::<u32>::new(self.io + REG_HOST_FEATURES).read() }
    }

    pub fn set_guest_features(&self, features: u32) {
        unsafe { Port::<u32>::new(self.io + REG_GUEST_FEATURES).write(features) }
    }

    /// Select a virtqueue and return its size (0 = queue doesn't exist)
    pub fn queue_size(&self, queue: u16) -> u16 {
        unsafe {
            Port::<u16>::new(self.io + REG_QUEUE_SEL).write(queue);
            Port::<u16>::new(self.io + REG_QUEUE_NUM).read()
        }
    }

    /// Register a virtqueue's ring memory (queue must be selected)
    pub fn set_queue_pfn(&self, queue: u16, phys: u64) {
        unsafe {
            Port::<u16>::new(self.io + REG_QUEUE_SEL).write(queue);
            Port::<u32>::new(self.io + REG_QUEUE_PFN).write((phys >> 12) as u32);
        }
    }

    /// Tell the device a queue has new buffers
    pub fn notify(&self, queue: u16) {
        unsafe { Port::<u16>::new(self.io + REG_QUEUE_NOTIFY).write(queue) }
    }

    /// Read + acknowledge the ISR status
    #[allow(dead_code)]
    pub fn isr(&self) -> u8 {
        unsafe { Port::<u8>::new(self.io + REG_ISR).read() }
    }

    /// Finish initialization — device is live after this
    pub fn driver_ok(&self) {
        self.write_status(STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_DRIVER_OK);
    }

    pub fn config_read8(&self, offset: u16) -> u8 {
        unsafe { Port::<u8>::new(self.io + REG_DEVICE_CONFIG + offset).read() }
    }

    pub fn config_read32(&self, offset: u16) -> u32 {
        unsafe { Port::<u32>::new(self.io + REG_DEVICE_CONFIG + offset).read() }
    }

    pub fn config_read64(&self, offset: u16) -> u64 {
        let lo = self.config_read32(offset) as u64;
        let hi = self.config_read32(offset + 4) as u64;
        (hi << 32) | lo
    }
}

// ========================================================================
// Split virtqueue (legacy layout)
// ========================================================================

/// Descriptor flags
const DESC_F_NEXT: u16 = 1;
const DESC_F_WRITE: u16 = 2; // device writes to this buffer

#[repr(C)]
#[derive(Clone, Copy)]
struct Desc {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

/// A buffer to hand to the device: (physical address, length,
/// device_writes). `device_writes=true` for buffers the device fills.
pub type QueueBuf = (u64, u32, bool);

/// Split virtqueue in the legacy layout:
///   page 0..: descriptor table, then avail ring
///   next 4 KiB boundary: used ring
pub struct Virtqueue {
    size: u16,
    desc: *mut Desc,
    avail_flags: *mut u16, // avail: flags, idx, ring[size]
    avail_idx: *mut u16,
    avail_ring: *mut u16,
    used_idx: *const u16, // used: flags, idx, ring[size] of {id u32, len u32}
    used_ring: *const [u32; 2],
    /// Head of the free-descriptor list (chained via Desc::next)
    free_head: u16,
    num_free: u16,
    last_used: u16,
    pub ring_phys: u64,
}

unsafe impl Send for Virtqueue {}

impl Virtqueue {
    /// Allocate and initialize a virtqueue of `size` entries
    pub fn new(size: u16) -> Self {
        let desc_bytes = size as usize * core::mem::size_of::<Desc>();
        let avail_bytes = 6 + 2 * size as usize;
        let used_offset = (desc_bytes + avail_bytes + 4095) & !4095;
        let used_bytes = 6 + 8 * size as usize;
        let total_pages = (used_offset + used_bytes + 4095) / 4096;

        let region = dma_alloc(total_pages);
        let base = region.virt;

        unsafe {
            let desc = base as *mut Desc;
            // Chain all descriptors into the free list
            for i in 0..size {
                (*desc.add(i as usize)).next = i + 1;
            }

            let avail = base.add(desc_bytes) as *mut u16;
            let used = base.add(used_offset) as *mut u16;

            Self {
                size,
                desc,
                avail_flags: avail,
                avail_idx: avail.add(1),
                avail_ring: avail.add(2),
                used_idx: used.add(1),
                used_ring: used.add(2) as *const [u32; 2],
                free_head: 0,
                num_free: size,
                last_used: 0,
                ring_phys: region.phys,
            }
        }
    }

    /// Add a descriptor chain and publish it in the avail ring.
    /// Returns the chain's head descriptor index, or None if out of
    /// descriptors. Caller must `notify()` the device afterwards.
    pub fn submit(&mut self, bufs: &[QueueBuf]) -> Option<u16> {
        if bufs.is_empty() || (bufs.len() as u16) > self.num_free {
            return None;
        }

        let head = self.free_head;
        let mut idx = head;
        unsafe {
            for (i, &(addr, len, device_writes)) in bufs.iter().enumerate() {
                let d = &mut *self.desc.add(idx as usize);
                d.addr = addr;
                d.len = len;
                d.flags = if device_writes { DESC_F_WRITE } else { 0 };
                if i + 1 < bufs.len() {
                    d.flags |= DESC_F_NEXT;
                    idx = d.next;
                } else {
                    let next_free = d.next;
                    d.next = 0;
                    self.free_head = next_free;
                }
            }
            self.num_free -= bufs.len() as u16;

            // Publish: write ring entry, fence, then bump avail idx
            let avail = self.avail_idx.read_volatile();
            self.avail_ring
                .add((avail % self.size) as usize)
                .write_volatile(head);
            fence(Ordering::SeqCst);
            self.avail_idx.write_volatile(avail.wrapping_add(1));
            self.avail_flags.write_volatile(0);
        }
        Some(head)
    }

    /// Reap one completion from the used ring, returning the chain head
    /// index and the number of bytes the device wrote.
    pub fn pop_used(&mut self, chain_len: u16) -> Option<(u16, u32)> {
        unsafe {
            if self.last_used == self.used_idx.read_volatile() {
                return None;
            }
            fence(Ordering::SeqCst);
            let slot = (self.last_used % self.size) as usize;
            let [id, len] = self.used_ring.add(slot).read_volatile();
            self.last_used = self.last_used.wrapping_add(1);

            // Return the chain to the free list
            let head = id as u16;
            let mut tail = head;
            for _ in 1..chain_len {
                tail = (*self.desc.add(tail as usize)).next;
            }
            (*self.desc.add(tail as usize)).next = self.free_head;
            self.free_head = head;
            self.num_free += chain_len;

            Some((head, len))
        }
    }

    /// Busy-wait for a completion, with a rough timeout in milliseconds
    pub fn wait_used(&mut self, chain_len: u16, timeout_ms: u32) -> Option<(u16, u32)> {
        for _ in 0..timeout_ms {
            if let Some(r) = self.pop_used(chain_len) {
                return Some(r);
            }
            crate::apic::pit_wait_ms(1);
        }
        self.pop_used(chain_len)
    }
}

// ========================================================================
// Init
// ========================================================================

/// Probe the PCI bus for virtio devices and bring up the drivers we have
pub fn init(devices: &[crate::pci::PciDevice]) {
    for dev in devices {
        if dev.vendor_id != VIRTIO_VENDOR_ID {
            continue;
        }
        match dev.device_id {
            // Transitional virtio-net / virtio-blk
            0x1000 => net::init(dev),
            0x1001 => blk::init(dev),
            other => {
                crate::serial::write_str("[VIRTIO] Unhandled virtio device 0x");
                crate::serial::write_hex(other as u64);
                crate::serial::write_str("\n");
            }
        }
    }
}
