//! Virtio — transport-independent core: DMA memory, split virtqueues,
//! and the `Transport` interface the device drivers (virtio-blk,
//! virtio-net) are written against.
//!
//! Transports live with the architecture: legacy virtio-pci over port I/O
//! on amd64, virtio-mmio (legacy v1 or modern v2) on arm64. Both use the
//! same split-virtqueue memory layout: descriptor table, avail ring, and a
//! page-aligned used ring in one physically contiguous allocation.

pub mod blk;
pub mod net;
pub mod pci;

use alloc::boxed::Box;
use core::sync::atomic::{fence, Ordering};

// Device status bits (common to all transports)
pub const STATUS_ACKNOWLEDGE: u8 = 1;
pub const STATUS_DRIVER: u8 = 2;
pub const STATUS_DRIVER_OK: u8 = 4;
pub const STATUS_FEATURES_OK: u8 = 8;

/// A virtio device transport
pub trait Transport: Send {
    /// Device feature bits 0-31
    fn host_features(&self) -> u32;
    /// Accept feature bits 0-31 (a modern transport also negotiates
    /// VIRTIO_F_VERSION_1 and completes the FEATURES_OK handshake)
    fn set_guest_features(&self, features: u32);
    /// Maximum size of queue `queue` (0 = queue doesn't exist)
    fn queue_max(&self, queue: u16) -> u16;
    /// Hand `vq`'s ring memory to the device as queue `queue`
    fn setup_queue(&self, queue: u16, vq: &Virtqueue);
    /// Tell the device a queue has new buffers
    fn notify(&self, queue: u16);
    /// Finish initialization — device is live after this
    fn driver_ok(&self);
    /// Device-specific configuration space
    fn config_read8(&self, offset: u16) -> u8;
    fn config_read32(&self, offset: u16) -> u32;
    fn config_read64(&self, offset: u16) -> u64 {
        let lo = self.config_read32(offset) as u64;
        let hi = self.config_read32(offset + 4) as u64;
        (hi << 32) | lo
    }
    /// Whether this is a modern (virtio 1.0) device — changes the
    /// virtio-net header size
    fn modern(&self) -> bool {
        false
    }
    /// Human-readable transport name for the boot log
    fn name(&self) -> &'static str;
}

/// A device found by the architecture's bus probe
pub fn attach(device_id: u32, transport: Box<dyn Transport>) {
    match device_id {
        1 => net::init(transport),
        2 => blk::init(transport),
        other => {
            crate::serial::write_str("[VIRTIO] Unhandled virtio device type ");
            crate::serial::write_dec(other as u64);
            crate::serial::write_str("
");
        }
    }
}

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

/// Allocate `pages` physically contiguous, zeroed pages for device DMA
pub fn dma_alloc(pages: usize) -> DmaRegion {
    let phys = crate::memory::heap::with_frame_allocator(|fa| fa.allocate_contiguous(pages))
        .expect("Frame allocator not initialized")
        .expect("OOM allocating DMA region");

    let virt = crate::arch::phys_to_virt(phys);
    let size = pages * 4096;
    unsafe { core::ptr::write_bytes(virt, 0, size) };

    DmaRegion { phys, virt, size }
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
    /// Physical address of the ring memory (descriptor table first)
    pub ring_phys: u64,
    /// Byte offsets of the avail and used rings within the ring memory
    avail_offset: u64,
    used_offset: u64,
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
                avail_offset: desc_bytes as u64,
                used_offset: used_offset as u64,
            }
        }
    }

    /// Number of entries
    pub fn size(&self) -> u16 {
        self.size
    }

    /// Physical addresses of the descriptor table, avail ring, and used
    /// ring (for transports that program them separately)
    pub fn ring_addrs(&self) -> (u64, u64, u64) {
        (
            self.ring_phys,
            self.ring_phys + self.avail_offset,
            self.ring_phys + self.used_offset,
        )
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
            crate::arch::delay_ms(1);
        }
        self.pop_used(chain_len)
    }
}

