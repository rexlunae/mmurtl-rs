//! Physical Frame Allocator — Manages 4 KiB physical memory frames.
//!
//! Architecture-neutral: the boot code hands in the list of usable RAM
//! ranges (from the bootloader's memory map on amd64, from the device tree
//! on arm64). A bitmap tracks every 4 KiB frame from physical address 0;
//! only frames inside a usable range start out free, so MMIO holes, the
//! kernel image, firmware tables, etc. are never handed out.
//!
//! Physical addresses are plain `u64`s; the CPU reaches a frame through
//! `crate::arch::phys_to_virt` (a high kernel window on both architectures).

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
    /// Free frames already scrubbed. Allocated frames always clear this bit.
    zeroed: &'static mut [u32; BITMAP_ENTRIES],
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
        let reserved_frames = 1 + (2 * bitmap_bytes + FRAME_SIZE - 1) / FRAME_SIZE; // header + bitmap
        let reserved_bytes = reserved_frames * FRAME_SIZE;

        let header_addr = usable
            .iter()
            .find_map(|r| {
                let mut start = (r.start.max(min_addr) + FRAME_SIZE - 1) & !(FRAME_SIZE - 1);
                loop {
                    let end = start.checked_add(reserved_bytes)?;
                    if end > r.end.min(MAX_PHYSICAL_MEMORY) { return None; }
                    if let Some(overlap) = reserved.iter().find(|x| start < x.end && end > x.start) {
                        start = (overlap.end.checked_add(FRAME_SIZE - 1)?) & !(FRAME_SIZE - 1);
                    } else { return Some(start); }
                }
            })
            .expect("No usable region large enough for the frame bitmap");

        // Clear padding in the metadata reservation too, before creating references.
        scrub(header_addr, reserved_bytes);
        let bitmap_addr = header_addr + FRAME_SIZE;
        let bitmap: &'static mut [u32; BITMAP_ENTRIES] =
            unsafe { &mut *(virt(bitmap_addr) as *mut [u32; BITMAP_ENTRIES]) };

        let zeroed: &'static mut [u32; BITMAP_ENTRIES] =
            unsafe { &mut *(virt(bitmap_addr + bitmap_bytes) as *mut [u32; BITMAP_ENTRIES]) };

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

        crate::serial::write_str("[FRAME] Bitmap at physical ");
        crate::kptr::write(bitmap_addr);
        crate::serial::write_str(", tracking ");
        crate::serial::write_dec(total_frames as u64);
        crate::serial::write_str(" frames, free=");
        crate::serial::write_dec(free_count as u64);
        crate::serial::write_str(" (");
        crate::serial::write_dec((free_count as u64 * FRAME_SIZE) / (1024 * 1024));
        crate::serial::write_str(" MiB)\n");

        crate::serial::write_line("[MEM] free memory scrubbed on release");
        let header = virt(header_addr) as *mut FrameAllocator;
        unsafe {
            header.write(FrameAllocator {
                bitmap,
                zeroed,
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
        if self.zeroed[idx / 32] & (1 << (idx % 32)) == 0 {
            scrub(idx as u64 * FRAME_SIZE, FRAME_SIZE);
        }
        self.zeroed[idx / 32] &= !(1 << (idx % 32));
        self.bitmap[idx / 32] |= 1 << (idx % 32);
    }

    /// Allocate a zeroed physical frame; returns its physical address
    pub fn allocate_zeroed_frame(&mut self) -> Option<u64> {
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
    pub fn allocate_zeroed_contiguous(&mut self, count: usize) -> Option<u64> {
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
        if start % FRAME_SIZE != 0 { return false; }
        let first = (start / FRAME_SIZE) as usize;
        if first > self.total_frames || count > self.total_frames.saturating_sub(first) || !(first..first + count).all(|i| self.is_free(i)) {
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
        assert!(phys % FRAME_SIZE == 0, "unaligned frame release");
        let idx = (phys / FRAME_SIZE) as usize;
        assert!(idx < self.total_frames, "frame release outside RAM");
        if !self.is_free(idx) {
            // Keep ownership until the entire frame is erased. The caller
            // must have removed mappings and stopped all CPU/DMA users.
            scrub(phys, FRAME_SIZE);
            self.zeroed[idx / 32] |= 1 << (idx % 32);
            self.bitmap[idx / 32] &= !(1 << (idx % 32));
            self.free_frames += 1;
        }
    }

    /// Best-effort shutdown hook: scrub all free RAM, including untouched
    /// firmware pages. Call only after CPUs/devices are quiesced. Live heap,
    /// stacks, page tables and other allocated frames must be wiped separately
    /// once their owners stop using them; never erase them under a running OS.
    #[allow(dead_code)]
    pub fn scrub_free_memory(&mut self) {
        for i in 0..self.total_frames {
            if self.is_free(i) {
                scrub(i as u64 * FRAME_SIZE, FRAME_SIZE);
                self.zeroed[i / 32] |= 1 << (i % 32);
            }
        }
    }

    /// Debug check of frames scrubbed on release. Firmware's untouched free
    /// frames are zeroed lazily on first allocation and excluded here.
    pub fn released_frames_are_zero(&self) -> bool {
        (0..self.total_frames).filter(|&i| self.is_free(i)
            && self.zeroed[i / 32] & (1 << (i % 32)) != 0).all(|i| {
                let ptr = virt(i as u64 * FRAME_SIZE) as *const u64;
                (0..FRAME_SIZE as usize / 8).all(|n| unsafe { ptr.add(n).read_volatile() == 0 })
            })
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

/// Volatile stores prevent dead-store removal when erasing released secrets.
/// All callers supply aligned, exclusively owned RAM through the physmap.
fn scrub(phys: u64, bytes: u64) {
    let ptr = virt(phys) as *mut u64;
    for n in 0..bytes as usize / 8 {
        unsafe { ptr.add(n).write_volatile(0); }
    }
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
}
