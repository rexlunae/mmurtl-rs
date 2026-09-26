//! Physical Frame Allocator — Manages 4 KiB physical memory frames.
//!
//! Architecture-neutral: the boot code hands in the list of usable RAM
//! ranges (from the bootloader's memory map on amd64, from the device tree
//! on arm64). A bitmap tracks every 4 KiB frame from physical address 0;
//! only frames inside a usable range start out free, so MMIO holes, the
//! kernel image, firmware tables, etc. are never handed out.
//!
//! Physical addresses are plain `u64`s; the CPU reaches a frame through
//! `crate::arch::phys_to_virt` (an offset window on amd64, identity on
//! arm64).

/// Size of one physical frame
pub const FRAME_SIZE: u64 = 4096; // 4 KiB

/// Maximum physical memory we support (32 GiB → 8,388,608 frames)
/// Bitmap at 1 bit/frame = ~1 MiB of bitmap data.
pub const MAX_PHYSICAL_MEMORY: u64 = 32 * 1024 * 1024 * 1024; // 32 GiB

/// Number of bitmap entries needed
const BITMAP_ENTRIES: usize = (MAX_PHYSICAL_MEMORY / FRAME_SIZE / 32) as usize;

/// A physical address range `[start, end)`
#[derive(Clone, Copy, Debug)]
pub struct PhysRange {
    pub start: u64,
    pub end: u64,
}

/// Bitmap-based frame allocator
pub struct FrameAllocator {
    /// Bitmap: 1 bit per frame (1 = allocated/used, 0 = free)
    bitmap: &'static mut [u32; BITMAP_ENTRIES],
    /// Number of frames covered by usable RAM (scan limit)
    total_frames: usize,
    /// Number of free frames
    free_frames: usize,
    /// Hint for faster allocation (last allocated frame index)
    last_search: usize,
}

fn virt(phys: u64) -> u64 {
    crate::arch::phys_to_virt(phys) as u64
}

/// Mark whole frames overlapping `[start, end)` used, or free only the
/// frames lying entirely inside it
fn set_range(bitmap: &mut [u32; BITMAP_ENTRIES], start: u64, end: u64, used: bool) {
    let end = end.min(MAX_PHYSICAL_MEMORY);
    let (first, last) = if used {
        (start / FRAME_SIZE, (end + FRAME_SIZE - 1) / FRAME_SIZE)
    } else {
        ((start + FRAME_SIZE - 1) / FRAME_SIZE, end / FRAME_SIZE)
    };
    for idx in first as usize..(last as usize).min(BITMAP_ENTRIES * 32) {
        if used {
            bitmap[idx / 32] |= 1 << (idx % 32);
        } else {
            bitmap[idx / 32] &= !(1 << (idx % 32));
        }
    }
}

impl FrameAllocator {
    /// Build the allocator from the usable RAM ranges, then mark `reserved`
    /// ranges (e.g. low memory, boot structures) as used. The allocator's
    /// own header + bitmap are placed in, and reserved from, the first
    /// usable range with room for them at or above `min_addr`.
    pub fn init(usable: &[PhysRange], reserved: &[PhysRange], min_addr: u64) -> &'static mut Self {
        let top = usable.iter().map(|r| r.end).max().unwrap_or(0).min(MAX_PHYSICAL_MEMORY);
        let total_frames = (top / FRAME_SIZE) as usize;

        let bitmap_bytes = BITMAP_ENTRIES as u64 * 4;
        let reserved_frames = 1 + (bitmap_bytes + FRAME_SIZE - 1) / FRAME_SIZE; // header + bitmap
        let reserved_bytes = reserved_frames * FRAME_SIZE;

        let header_addr = usable
            .iter()
            .find_map(|r| {
                let start = (r.start.max(min_addr) + FRAME_SIZE - 1) & !(FRAME_SIZE - 1);
                (start + reserved_bytes <= r.end).then_some(start)
            })
            .expect("No usable region large enough for the frame bitmap");

        let bitmap_addr = header_addr + FRAME_SIZE;
        let bitmap: &'static mut [u32; BITMAP_ENTRIES] =
            unsafe { &mut *(virt(bitmap_addr) as *mut [u32; BITMAP_ENTRIES]) };

        // Everything starts used; only usable ranges are freed
        bitmap.fill(u32::MAX);
        for r in usable {
            set_range(bitmap, r.start, r.end, false);
        }
        for r in reserved {
            set_range(bitmap, r.start, r.end, true);
        }
        // The header + bitmap's own frames
        set_range(bitmap, header_addr, header_addr + reserved_bytes, true);

        let free_count = (0..total_frames)
            .filter(|&i| bitmap[i / 32] & (1 << (i % 32)) == 0)
            .count();

        crate::serial::write_str("[FRAME] Bitmap at physical 0x");
        crate::serial::write_hex(bitmap_addr);
        crate::serial::write_str(", tracking ");
        crate::serial::write_dec(total_frames as u64);
        crate::serial::write_str(" frames, free=");
        crate::serial::write_dec(free_count as u64);
        crate::serial::write_str(" (");
        crate::serial::write_dec((free_count as u64 * FRAME_SIZE) / (1024 * 1024));
        crate::serial::write_str(" MiB)\n");

        let header = virt(header_addr) as *mut FrameAllocator;
        unsafe {
            header.write(FrameAllocator {
                bitmap,
                total_frames,
                free_frames: free_count,
                last_search: 0,
            });
            &mut *header
        }
    }

    fn is_free(&self, idx: usize) -> bool {
        self.bitmap[idx / 32] & (1 << (idx % 32)) == 0
    }

    fn mark(&mut self, idx: usize) {
        self.bitmap[idx / 32] |= 1 << (idx % 32);
    }

    /// Allocate a single physical frame; returns its physical address
    pub fn allocate_frame(&mut self) -> Option<u64> {
        let total = self.total_frames;
        let start = self.last_search;
        for i in 0..total {
            let idx = (start + i) % total;
            if self.is_free(idx) {
                self.mark(idx);
                self.free_frames -= 1;
                self.last_search = idx + 1;
                return Some(idx as u64 * FRAME_SIZE);
            }
        }
        crate::serial::write_str("[FRAME] Out of memory!\n");
        None
    }

    /// Allocate `count` physically contiguous frames (for DMA rings,
    /// buffers, and the arm64 heap). Returns the first frame's address.
    pub fn allocate_contiguous(&mut self, count: usize) -> Option<u64> {
        if count == 0 {
            return None;
        }
        let mut run_start = 0usize;
        let mut run_len = 0usize;
        for idx in 0..self.total_frames {
            if self.is_free(idx) {
                if run_len == 0 {
                    run_start = idx;
                }
                run_len += 1;
                if run_len == count {
                    for i in run_start..run_start + count {
                        self.mark(i);
                    }
                    self.free_frames -= count;
                    return Some(run_start as u64 * FRAME_SIZE);
                }
            } else {
                run_len = 0;
            }
        }
        crate::serial::write_str("[FRAME] No contiguous run found!\n");
        None
    }

    /// Claim the specific frames `[start, start + count * FRAME_SIZE)` if
    /// they are all free (used to grow a physically contiguous heap in
    /// place). All-or-nothing.
    #[allow(dead_code)]
    pub fn claim_range(&mut self, start: u64, count: usize) -> bool {
        let first = (start / FRAME_SIZE) as usize;
        if first + count > self.total_frames || !(first..first + count).all(|i| self.is_free(i)) {
            return false;
        }
        for i in first..first + count {
            self.mark(i);
        }
        self.free_frames -= count;
        true
    }

    /// Free a previously allocated frame
    #[allow(dead_code)]
    pub fn deallocate_frame(&mut self, phys: u64) {
        let idx = (phys / FRAME_SIZE) as usize;
        if !self.is_free(idx) {
            self.bitmap[idx / 32] &= !(1 << (idx % 32));
            self.free_frames += 1;
        }
    }

    /// Number of free frames remaining
    pub fn free_count(&self) -> usize {
        self.free_frames
    }

    /// Number of frames covered by usable RAM
    #[allow(dead_code)]
    pub fn total_count(&self) -> usize {
        self.total_frames
    }
}
