//! Physical Frame Allocator — Manages 4 KiB physical memory frames.
//!
//! On boot, the bootloader provides a memory map. We build a bitmap tracking
//! every 4 KiB frame in physical memory. Frames marked as usable can be
//! allocated; all other regions (kernel, bootloader, MMIO, ACPI, etc.) are
//! marked as used.
//!
//! IMPORTANT: All physical addresses are converted to virtual addresses using
//! `phys_to_virt()` + physical memory offset, since the kernel runs in the
//! higher half and physical addresses are not identity-mapped.

use bootloader_api::info::{MemoryRegionKind, MemoryRegions};
use x86_64::PhysAddr;
use x86_64::structures::paging::{PhysFrame, Size4KiB};

/// Convert a physical address to a kernel-accessible virtual address
fn phys_to_virt(phys: u64) -> u64 {
    phys + crate::memory::page_table::physical_memory_offset()
}

/// Size of one physical frame
pub const FRAME_SIZE: u64 = 4096; // 4 KiB

/// Maximum physical memory we support (32 GiB → 8,388,608 frames)
/// Bitmap at 1 bit/frame = ~1 MiB of bitmap data.
pub const MAX_PHYSICAL_MEMORY: u64 = 32 * 1024 * 1024 * 1024; // 32 GiB

/// Number of bitmap entries needed
const BITMAP_ENTRIES: usize = (MAX_PHYSICAL_MEMORY / FRAME_SIZE / 32) as usize;

/// Bitmap-based frame allocator
pub struct FrameAllocator {
    /// Bitmap: 1 bit per frame (1 = allocated/used, 0 = free)
    bitmap: &'static mut [u32; BITMAP_ENTRIES],
    /// Total number of physical frames tracked
    total_frames: usize,
    /// Number of free frames
    free_frames: usize,
    /// Hints for faster allocation (last allocated frame index)
    last_search: usize,
}

impl FrameAllocator {
    /// Initialize the frame allocator from the bootloader's memory map.
    ///
    /// The bitmap itself is placed at the start of the first usable memory region.
    /// All physical addresses are converted to virtual via phys_to_virt.
    pub fn init(memory_regions: &MemoryRegions) -> &'static mut Self {
        let (bitmap_addr, _bitmap_frames, total_frames) = Self::build_bitmap(memory_regions);

        // Convert bitmap physical address to accessible virtual address
        let bitmap_virt = phys_to_virt(bitmap_addr);
        let bitmap_ptr = bitmap_virt as *mut u32;
        let bitmap_slice = unsafe {
            core::slice::from_raw_parts_mut(bitmap_ptr, BITMAP_ENTRIES)
        };

        // Zero out the entire bitmap
        for entry in bitmap_slice.iter_mut() {
            *entry = 0;
        }

        // Mark all frames beyond the actual RAM as "used" (so they can't be allocated)
        let actual_frames = (Self::detect_top_of_ram(memory_regions) / FRAME_SIZE) as usize;
        if actual_frames < total_frames {
            for frame_idx in actual_frames..total_frames {
                let word = frame_idx / 32;
                let bit = frame_idx % 32;
                bitmap_slice[word] |= 1 << bit;
            }
        }

        // Mark all non-usable regions as used
        Self::mark_used(memory_regions, bitmap_slice);

        // Count free frames
        let mut free_count = 0;
        for frame_idx in 0..actual_frames {
            let word = frame_idx / 32;
            let bit = frame_idx % 32;
            if bitmap_slice[word] & (1 << bit) == 0 {
                free_count += 1;
            }
        }

        serial_write_str("[FRAME] Bitmap at physical 0x");
        serial_write_hex(bitmap_addr);
        serial_write_str(" (virt 0x");
        serial_write_hex(bitmap_virt);
        serial_write_str("), total=");
        serial_write_dec(total_frames as u64);
        serial_write_str(", free=");
        serial_write_dec(free_count as u64);
        serial_write_str(" (");
        serial_write_dec((free_count as u64 * FRAME_SIZE) / (1024 * 1024));
        serial_write_str(" MiB)\n");

        // Place the FrameAllocator header at the start of the bitmap region (phys + offset)
        let allocator_virt = bitmap_virt as *mut FrameAllocator;
        unsafe {
            allocator_virt.write(FrameAllocator {
                bitmap: core::mem::transmute(bitmap_slice.as_mut_ptr()),
                total_frames,
                free_frames: free_count,
                last_search: 0,
            });
            &mut *allocator_virt
        }
    }

    /// Find the top of usable RAM (highest usable address)
    fn detect_top_of_ram(memory_regions: &MemoryRegions) -> u64 {
        let mut top = 0u64;
        for region in memory_regions.iter() {
            if region.kind == MemoryRegionKind::Usable {
                if region.end > top {
                    top = region.end;
                }
            }
        }
        top
    }

    /// Calculate bitmap size and find placement
    fn build_bitmap(memory_regions: &MemoryRegions) -> (u64, u64, usize) {
        let total_frames = (MAX_PHYSICAL_MEMORY / FRAME_SIZE) as usize;
        let bitmap_bytes = (total_frames + 7) / 8;
        let bitmap_frames = (bitmap_bytes as u64 + FRAME_SIZE - 1) / FRAME_SIZE;

        // Find a spot for the bitmap: use the start of the first usable region
        let bitmap_addr = memory_regions
            .iter()
            .find(|r| r.kind == MemoryRegionKind::Usable)
            .map(|r| r.start)
            .unwrap_or(0x100_0000); // Fallback: 16 MiB

        (bitmap_addr, bitmap_frames, total_frames)
    }

    /// Mark all non-usable frames as allocated in the bitmap
    fn mark_used(memory_regions: &MemoryRegions, bitmap: &mut [u32]) {
        for region in memory_regions.iter() {
            if region.kind != MemoryRegionKind::Usable {
                let start_frame = (region.start / FRAME_SIZE) as usize;
                let end_frame = ((region.end + FRAME_SIZE - 1) / FRAME_SIZE) as usize;
                for frame_idx in start_frame..end_frame {
                    if frame_idx < BITMAP_ENTRIES * 32 {
                        let word = frame_idx / 32;
                        let bit = frame_idx % 32;
                        bitmap[word] |= 1 << bit;
                    }
                }
            }
        }
    }

    /// Allocate a single physical frame (returns None if OOM)
    pub fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        let total_frames = self.total_frames;
        let start = self.last_search;

        for i in 0..total_frames {
            let frame_idx = (start + i) % total_frames;
            let word = frame_idx / 32;
            let bit = frame_idx % 32;
            if self.bitmap[word] & (1 << bit) == 0 {
                // Found a free frame — mark it used
                self.bitmap[word] |= 1 << bit;
                self.free_frames -= 1;
                self.last_search = frame_idx + 1;
                let phys_addr = PhysAddr::new(frame_idx as u64 * FRAME_SIZE);
                return Some(PhysFrame::<Size4KiB>::containing_address(phys_addr));
            }
        }

        serial_write_str("[FRAME] Out of memory!\n");
        None
    }

    /// Free a previously allocated frame
    pub fn deallocate_frame(&mut self, frame: PhysFrame<Size4KiB>) {
        let addr = frame.start_address().as_u64();
        let frame_idx = (addr / FRAME_SIZE) as usize;
        let word = frame_idx / 32;
        let bit = frame_idx % 32;

        if self.bitmap[word] & (1 << bit) != 0 {
            self.bitmap[word] &= !(1 << bit);
            self.free_frames += 1;
        }
    }

    /// Return the number of free frames remaining
    pub fn free_count(&self) -> usize {
        self.free_frames
    }

    /// Return total frames tracked
    pub fn total_count(&self) -> usize {
        self.total_frames
    }
}

// ========================================================================
// x86_64 crate FrameAllocator trait implementation
// ========================================================================

/// Adapter that wraps our FrameAllocator for the x86_64 crate's paging API
pub struct BumpFrameAllocator {
    inner: *mut FrameAllocator,
}

impl BumpFrameAllocator {
    pub fn new(inner: &mut FrameAllocator) -> Self {
        Self { inner: inner as *mut FrameAllocator }
    }
}

unsafe impl x86_64::structures::paging::FrameAllocator<Size4KiB> for BumpFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        unsafe { (*self.inner).allocate_frame() }
    }
}

// ========================================================================
// Inline serial helpers (avoid circular deps)
// ========================================================================

fn serial_write_str(s: &str) {
    crate::serial::write_str(s);
}

fn serial_write_hex(v: u64) {
    crate::serial::write_hex(v);
}

fn serial_write_dec(v: u64) {
    crate::serial::write_dec(v);
}
