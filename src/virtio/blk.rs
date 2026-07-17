//! virtio-blk — block storage driver.
//!
//! One virtqueue (queue 0). Each request is a 3-descriptor chain:
//!   [request header: type + sector] (driver → device)
//!   [data: 512 × n bytes]           (direction depends on read/write)
//!   [status byte]                   (device → driver)
//!
//! Requests are synchronous: submit, notify, poll the used ring. Fine for
//! a first storage driver; interrupt-driven completion comes with MSI-X.

use spin::Mutex;

use super::{DmaRegion, VirtioLegacy, Virtqueue};

pub const SECTOR_SIZE: usize = 512;

const REQ_TYPE_READ: u32 = 0;
const REQ_TYPE_WRITE: u32 = 1;

/// Max sectors per request (bounded by our single-page data buffer)
const MAX_SECTORS_PER_REQ: usize = 8;

struct BlkDevice {
    transport: VirtioLegacy,
    queue: Virtqueue,
    /// One page: request header (16 B) + status byte (offset 16)
    req: DmaRegion,
    /// One page: data buffer (up to 8 sectors)
    data: DmaRegion,
    capacity_sectors: u64,
    /// Poisoned after a timeout or mismatched completion. A timed-out
    /// request leaves the device owning our descriptors AND our shared
    /// req/data DMA buffers — it may complete (and DMA) at any later
    /// moment, so reusing the buffers or trusting the next used-ring
    /// entry would misattribute a stale completion to a new request.
    /// Recovery requires a full device reset; until then, fail fast.
    failed: bool,
}

static BLK: Mutex<Option<BlkDevice>> = Mutex::new(None);

/// Whether a virtio-blk device is available (and not poisoned by a
/// timed-out request)
pub fn available() -> bool {
    BLK.lock().as_ref().map_or(false, |d| !d.failed)
}

/// Device capacity in 512-byte sectors (0 if no device)
pub fn capacity_sectors() -> u64 {
    BLK.lock().as_ref().map_or(0, |d| d.capacity_sectors)
}

/// Initialize a transitional virtio-blk PCI device
pub fn init(dev: &crate::pci::PciDevice) {
    let transport = match VirtioLegacy::new(dev) {
        Some(t) => t,
        None => {
            crate::serial::write_line("[BLK] virtio-blk has no I/O BAR — skipped");
            return;
        }
    };

    // No features needed for basic reads/writes
    let _host = transport.host_features();
    transport.set_guest_features(0);

    let qsize = transport.queue_size(0);
    if qsize == 0 {
        crate::serial::write_line("[BLK] Queue 0 missing — skipped");
        return;
    }
    let queue = Virtqueue::new(qsize);
    transport.set_queue_pfn(0, queue.ring_phys);
    transport.driver_ok();

    // Device config: capacity (le64) at offset 0
    let capacity_sectors = transport.config_read64(0);

    crate::serial::write_str("[BLK] virtio-blk ready: ");
    crate::serial::write_dec(capacity_sectors);
    crate::serial::write_str(" sectors (");
    crate::serial::write_dec(capacity_sectors * SECTOR_SIZE as u64 / 1024);
    crate::serial::write_str(" KiB), queue size ");
    crate::serial::write_dec(qsize as u64);
    crate::serial::write_str("\n");

    *BLK.lock() = Some(BlkDevice {
        transport,
        queue,
        req: super::dma_alloc(1),
        data: super::dma_alloc(1),
        capacity_sectors,
        failed: false,
    });
}

/// Perform one request against the device. Returns false on any error.
fn do_request(dev: &mut BlkDevice, write: bool, sector: u64, buf: &mut [u8]) -> bool {
    if dev.failed {
        return false;
    }
    let sectors = buf.len() / SECTOR_SIZE;
    assert!(
        sectors >= 1 && sectors <= MAX_SECTORS_PER_REQ && buf.len() % SECTOR_SIZE == 0,
        "virtio-blk: buffer must be 1-8 whole sectors"
    );
    if sector + sectors as u64 > dev.capacity_sectors {
        return false;
    }

    unsafe {
        // Request header: type (u32), reserved (u32), sector (u64)
        let hdr = dev.req.virt as *mut u32;
        hdr.write_volatile(if write { REQ_TYPE_WRITE } else { REQ_TYPE_READ });
        hdr.add(1).write_volatile(0);
        (dev.req.virt.add(8) as *mut u64).write_volatile(sector);
        // Status byte: preset to an invalid value so we can detect writes
        dev.req.virt.add(16).write_volatile(0xFF);

        if write {
            core::ptr::copy_nonoverlapping(buf.as_ptr(), dev.data.virt, buf.len());
        }
    }

    let chain = [
        (dev.req.phys, 16, false),
        (dev.data.phys, buf.len() as u32, !write),
        (dev.req.phys + 16, 1, true),
    ];
    let head = match dev.queue.submit(&chain) {
        Some(h) => h,
        None => return false,
    };
    dev.transport.notify(0);

    match dev.queue.wait_used(3, 1000) {
        None => {
            // Descriptors and DMA buffers are still device-owned; a late
            // completion would be misread by the next request. Poison.
            dev.failed = true;
            crate::serial::write_line("[BLK] Request timed out — device disabled (needs reset)");
            return false;
        }
        Some((used_head, _len)) if used_head != head => {
            // A completion for a chain we didn't just submit can only be
            // a stale one from a previous failure — never trust it.
            dev.failed = true;
            crate::serial::write_line("[BLK] Stale completion detected — device disabled");
            return false;
        }
        Some(_) => {}
    }

    let status = unsafe { dev.req.virt.add(16).read_volatile() };
    if status != 0 {
        return false;
    }

    if !write {
        unsafe {
            core::ptr::copy_nonoverlapping(dev.data.virt, buf.as_mut_ptr(), buf.len());
        }
    }
    true
}

/// Read whole sectors into `buf` (len must be a multiple of 512, ≤ 4 KiB)
pub fn read_sectors(sector: u64, buf: &mut [u8]) -> bool {
    let mut guard = BLK.lock();
    match guard.as_mut() {
        Some(dev) => do_request(dev, false, sector, buf),
        None => false,
    }
}

/// Write whole sectors from `buf` (len must be a multiple of 512, ≤ 4 KiB)
pub fn write_sectors(sector: u64, buf: &mut [u8]) -> bool {
    let mut guard = BLK.lock();
    match guard.as_mut() {
        Some(dev) => do_request(dev, true, sector, buf),
        None => false,
    }
}

/// Boot-time self-test: write a signature to the last sector, read it
/// back, verify, then restore the sector's original contents (the disk
/// may hold a real filesystem).
pub fn self_test() {
    if !available() {
        crate::serial::write_line("[BLK] No virtio-blk device — self-test skipped");
        return;
    }
    let last = capacity_sectors() - 1;

    let mut original = [0u8; SECTOR_SIZE];
    if !read_sectors(last, &mut original) {
        crate::serial::write_line("[BLK] Self-test READ FAILED");
        return;
    }

    let mut wbuf = [0u8; SECTOR_SIZE];
    let sig = b"MMURTL/RS phase-7 virtio-blk self-test";
    wbuf[..sig.len()].copy_from_slice(sig);
    for (i, b) in wbuf[64..128].iter_mut().enumerate() {
        *b = i as u8;
    }

    if !write_sectors(last, &mut wbuf) {
        crate::serial::write_line("[BLK] Self-test WRITE FAILED");
        return;
    }

    let mut rbuf = [0u8; SECTOR_SIZE];
    if !read_sectors(last, &mut rbuf) {
        crate::serial::write_line("[BLK] Self-test READ FAILED");
        return;
    }

    // Put the original contents back before reporting
    let restored = write_sectors(last, &mut original);

    if rbuf == wbuf && restored {
        crate::serial::write_str("[BLK] Self-test OK: wrote+verified+restored sector ");
        crate::serial::write_dec(last);
        crate::serial::write_str("\n");
    } else {
        crate::serial::write_line("[BLK] Self-test MISMATCH");
    }
}
