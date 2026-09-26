//! amd64 memory glue: bootloader memory map → frame allocator, MMIO and
//! trampoline mappings, the heap's virtual region, and user-page mapping
//! for the portable `memory::user` module.

use bootloader_api::info::{MemoryRegionKind, MemoryRegions};
use x86_64::structures::paging::page_table::PageTableFlags as Flags;
use x86_64::structures::paging::{Page, PhysFrame};
use x86_64::{PhysAddr, VirtAddr};

use super::page_table;
use crate::memory::frame_allocator::{FrameAllocator, PhysRange, FRAME_SIZE};

/// Hand the bootloader's usable regions to the portable memory manager
pub fn init(regions: &MemoryRegions, phys_offset: Option<u64>) {
    page_table::init(phys_offset);

    let mut usable = [PhysRange { start: 0, end: 0 }; 64];
    let mut n = 0;
    for r in regions.iter().filter(|r| r.kind == MemoryRegionKind::Usable) {
        if n < usable.len() {
            usable[n] = PhysRange { start: r.start, end: r.end };
            n += 1;
        }
    }
    // Low memory: real-mode IVT, BDA, EBDA, and the SMP AP trampoline at
    // 0x8000 all live in the first MiB
    let reserved = [PhysRange { start: 0, end: 0x10_0000 }];
    crate::memory::init(&usable[..n], &reserved, 0x10_0000);
    kernel_root(); // capture the boot page tables as the kernel's space
}

/// Physical → kernel virtual, through the bootloader's offset window
pub fn phys_to_virt(phys: u64) -> *mut u8 {
    (phys + page_table::physical_memory_offset()) as *mut u8
}

// ========================================================================
// Kernel heap: a higher-half region grown page by page
// ========================================================================

/// Heap start virtual address (in the kernel's higher-half region)
const HEAP_START: u64 = 0xFFFF_9000_0000_0000;
/// Size of the initial kernel heap (4 MiB)
const INITIAL_HEAP_SIZE: u64 = 4 * 1024 * 1024;
/// Growth granularity (2 MiB)
const HEAP_GROW_CHUNK: u64 = 512 * FRAME_SIZE;

/// Map the initial heap; returns its [start, end)
pub fn heap_init(fa: &mut FrameAllocator) -> (u64, u64) {
    map_heap_pages(HEAP_START, INITIAL_HEAP_SIZE, fa);
    (HEAP_START, HEAP_START + INITIAL_HEAP_SIZE)
}

/// Grow the heap past `end` by at least `needed` bytes; returns the bytes
/// added. Called under the frame-allocator lock.
pub fn heap_extend(end: u64, needed: u64, fa: &mut FrameAllocator) -> Option<u64> {
    let grow = (needed + HEAP_GROW_CHUNK - 1) / HEAP_GROW_CHUNK * HEAP_GROW_CHUNK;
    map_heap_pages(end, grow, fa);
    Some(grow)
}

fn map_heap_pages(virt_start: u64, size: u64, fa: &mut FrameAllocator) {
    let num_pages = (size + FRAME_SIZE - 1) / FRAME_SIZE;
    let start_page = Page::containing_address(VirtAddr::new(virt_start));
    let mut adapter = page_table::BumpFrameAllocator::new(fa);
    for i in 0..num_pages {
        let pa = fa.allocate_frame().expect("OOM during heap page allocation");
        unsafe {
            page_table::map_page(
                start_page + i,
                PhysFrame::containing_address(PhysAddr::new(pa)),
                Flags::PRESENT | Flags::WRITABLE | Flags::NO_EXECUTE,
                &mut adapter,
            )
            .expect("Failed to map heap page");
        }
    }
}

// ========================================================================
// User pages (for memory::user)
// ========================================================================

/// PML4 slot holding the user window (memory::user::USER_BASE)
const USER_PML4_SLOT: usize = ((crate::memory::user::USER_BASE >> 39) & 0x1FF) as usize;

const PTE_PRESENT: u64 = 1 << 0;
const PTE_WRITABLE: u64 = 1 << 1;
const PTE_USER: u64 = 1 << 2;
const PTE_HUGE: u64 = 1 << 7;
const PTE_NX: u64 = 1 << 63;
const PTE_ADDR: u64 = 0x000F_FFFF_FFFF_F000;

/// The kernel's own PML4 (the bootloader's), captured at boot
static KERNEL_ROOT: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
/// Root currently loaded in each CPU's CR3
static ACTIVE_ROOT: [core::sync::atomic::AtomicU64; crate::scheduler::MAX_CPUS] =
    [const { core::sync::atomic::AtomicU64::new(0) }; crate::scheduler::MAX_CPUS];

fn kernel_root() -> u64 {
    use core::sync::atomic::Ordering;
    let r = KERNEL_ROOT.load(Ordering::Relaxed);
    if r != 0 {
        return r;
    }
    let (frame, _) = x86_64::registers::control::Cr3::read();
    let r = frame.start_address().as_u64();
    KERNEL_ROOT.store(r, Ordering::Relaxed);
    r
}

fn table(phys: u64) -> *mut u64 {
    phys_to_virt(phys) as *mut u64
}

fn alloc_zeroed_frame() -> Option<u64> {
    let pa = crate::memory::heap::with_frame_allocator(|fa| fa.allocate_frame())??;
    unsafe { core::ptr::write_bytes(phys_to_virt(pa), 0, FRAME_SIZE as usize) };
    Some(pa)
}

/// Create a user address space: a fresh PML4 sharing every kernel entry
/// (so kernel code, heap, and the physical window look identical in all
/// spaces) with its own, initially empty, user-window slot. Returns the
/// PML4's physical address.
pub fn new_address_space() -> Result<u64, &'static str> {
    let root = alloc_zeroed_frame().ok_or("out of memory for a page table")?;
    let kernel = table(kernel_root());
    let new = table(root);
    for i in 0..512 {
        if i != USER_PML4_SLOT {
            unsafe { *new.add(i) = *kernel.add(i) };
        }
    }
    Ok(root)
}

/// Walk `root` to the PTE for `va`, creating user-accessible tables
unsafe fn walk_create(root: u64, va: u64) -> Option<*mut u64> {
    let mut t = table(root);
    for level in (1..4).rev() {
        let e = t.add(((va >> (12 + 9 * level)) & 0x1FF) as usize);
        if *e & PTE_PRESENT == 0 {
            let pa = alloc_zeroed_frame()?;
            *e = pa | PTE_PRESENT | PTE_WRITABLE | PTE_USER;
        } else if *e & PTE_HUGE != 0 {
            return None;
        }
        t = table(*e & PTE_ADDR);
    }
    Some(t.add(((va >> 12) & 0x1FF) as usize))
}

/// Map one 4 KiB user page `va` → `pa` in address space `root`
pub fn map_user_page(root: u64, va: u64, pa: u64, writable: bool, executable: bool) -> Result<(), &'static str> {
    let mut pte = (pa & PTE_ADDR) | PTE_PRESENT | PTE_USER;
    if writable {
        pte |= PTE_WRITABLE;
    }
    if !executable {
        pte |= PTE_NX;
    }
    unsafe {
        let e = walk_create(root, va).ok_or("out of memory for page tables")?;
        if *e & PTE_PRESENT != 0 {
            return Err("page already mapped");
        }
        *e = pte;
    }
    Ok(())
}

/// Tear down a user address space: free every frame mapped in its user
/// window, the window's page tables, and the PML4 itself. Returns the
/// number of frames freed. The space must not be loaded on any CPU.
pub fn free_address_space(root: u64) -> usize {
    unsafe fn free_level(t: u64, level: u32, freed: &mut usize) {
        for i in 0..512 {
            let e = *table(t).add(i);
            if e & PTE_PRESENT == 0 {
                continue;
            }
            if level > 1 {
                free_level(e & PTE_ADDR, level - 1, freed);
            }
            free_frame(e & PTE_ADDR);
            *freed += 1;
        }
    }
    let mut freed = 0;
    unsafe {
        let top = *table(root).add(USER_PML4_SLOT);
        if top & PTE_PRESENT != 0 {
            free_level(top & PTE_ADDR, 3, &mut freed);
            free_frame(top & PTE_ADDR);
            freed += 1;
        }
    }
    free_frame(root);
    freed + 1
}

fn free_frame(pa: u64) {
    crate::memory::heap::with_frame_allocator(|fa| fa.deallocate_frame(pa));
}

/// Load `root` (0 = the kernel's own tables) into this CPU's CR3, if it
/// isn't loaded already. Kernel mappings are identical in every space, so
/// this is safe from kernel code at any point.
pub fn switch_address_space(cpu: usize, root: u64) {
    use core::sync::atomic::Ordering;
    let target = if root == 0 { kernel_root() } else { root };
    if ACTIVE_ROOT[cpu].load(Ordering::Relaxed) == target {
        return;
    }
    let (_, flags) = x86_64::registers::control::Cr3::read();
    unsafe {
        x86_64::registers::control::Cr3::write(
            PhysFrame::containing_address(PhysAddr::new(target)),
            flags,
        );
    }
    ACTIVE_ROOT[cpu].store(target, Ordering::Relaxed);
}

/// Whether `va` is mapped in the *current* address space: None if not,
/// else (user-accessible, writable)
pub fn query_page(va: u64) -> Option<(bool, bool)> {
    page_table::query_page(VirtAddr::new(va)).map(|f| {
        (f.contains(Flags::USER_ACCESSIBLE), f.contains(Flags::WRITABLE))
    })
}

/// If the CPU enforces SMAP (CR4.SMAP), deliberate kernel accesses to user
/// pages must be bracketed by STAC/CLAC. Without SMAP these instructions
/// would #UD, so they are only issued when the feature is on.
fn smap_enabled() -> bool {
    use x86_64::registers::control::{Cr4, Cr4Flags};
    Cr4::read().contains(Cr4Flags::SUPERVISOR_MODE_ACCESS_PREVENTION)
}

/// Open a window for the kernel to touch user memory
pub fn user_access_begin() {
    if smap_enabled() {
        unsafe { core::arch::asm!("stac", options(nomem, nostack)) };
    }
}

/// Close the user-memory window
pub fn user_access_end() {
    if smap_enabled() {
        unsafe { core::arch::asm!("clac", options(nomem, nostack)) };
    }
}

// ========================================================================
// MMIO / trampoline mappings
// ========================================================================

/// Ensure a physical range is accessible through the physical-memory offset
/// mapping, mapping any missing pages on demand.
///
/// With `uncached` (for MMIO: Local APIC, I/O APIC) the pages are also
/// guaranteed to be uncacheable. That matters even when the range is
/// *already* mapped: the bootloader's `Mapping::Dynamic` window covers
/// 0..max_phys_addr as cacheable write-back, so on machines with enough
/// RAM the MMIO window is pre-mapped with the wrong attributes (and often
/// inside a huge page). Such pages get their attributes forced to UC,
/// splitting the covering huge page so only the 4 KiB MMIO page changes.
pub fn ensure_phys_mapped(phys_start: u64, len: u64, uncached: bool) {
    use x86_64::structures::paging::{Page, PhysFrame};
    use x86_64::structures::paging::page_table::PageTableFlags as Flags;
    use x86_64::{PhysAddr, VirtAddr};

    let start = phys_start & !0xFFF;
    let end = (phys_start + len + 0xFFF) & !0xFFF;

    let mut phys = start;
    while phys < end {
        let virt = page_table::phys_to_virt(PhysAddr::new(phys));
        let already_mapped = page_table::translate_virtual(virt).is_some();

        if !already_mapped {
            let mut flags = Flags::PRESENT | Flags::WRITABLE | Flags::NO_EXECUTE;
            if uncached {
                flags |= Flags::NO_CACHE | Flags::WRITE_THROUGH;
            }
            crate::memory::heap::with_frame_allocator(|fa| {
                let mut adapter = page_table::BumpFrameAllocator::new(fa);
                unsafe {
                    page_table::map_page(
                        Page::containing_address(VirtAddr::new(virt.as_u64())),
                        PhysFrame::containing_address(PhysAddr::new(phys)),
                        flags,
                        &mut adapter,
                    )
                    .expect("Failed to map physical range");
                }
            })
            .expect("Frame allocator not initialized");
        } else if uncached && !page_is_uncached(virt) {
            crate::memory::heap::with_frame_allocator(|fa| {
                let mut adapter = page_table::BumpFrameAllocator::new(fa);
                unsafe {
                    page_table::set_page_uncached(virt, &mut adapter)
                        .expect("Failed to force UC on mapped MMIO page");
                }
            })
            .expect("Frame allocator not initialized");

            crate::serial::write_str("[PAGING] Forced UC on pre-mapped MMIO page phys 0x");
            crate::serial::write_hex(phys);
            crate::serial::write_str("\n");
        }
        phys += 4096;
    }
}

/// Whether the mapping covering `virt` already has cache-disable set
fn page_is_uncached(virt: x86_64::VirtAddr) -> bool {
    use x86_64::structures::paging::page_table::PageTableFlags as Flags;
    match page_table::query_page(virt) {
        Some(flags) => flags.contains(Flags::NO_CACHE),
        None => false,
    }
}

/// Identity-map a single executable page (virt == phys).
///
/// Used for the SMP AP trampoline: after an AP enables paging with the
/// kernel's CR3, it is still executing at its low physical address, so that
/// address must be identity-mapped and executable.
pub fn identity_map_executable(phys: u64) {
    use x86_64::structures::paging::{Page, PhysFrame};
    use x86_64::structures::paging::page_table::PageTableFlags as Flags;
    use x86_64::{PhysAddr, VirtAddr};

    if page_table::translate_virtual(VirtAddr::new(phys)).is_some() {
        return;
    }
    crate::memory::heap::with_frame_allocator(|fa| {
        let mut adapter = page_table::BumpFrameAllocator::new(fa);
        unsafe {
            page_table::map_page(
                Page::containing_address(VirtAddr::new(phys)),
                PhysFrame::containing_address(PhysAddr::new(phys)),
                Flags::PRESENT | Flags::WRITABLE,
                &mut adapter,
            )
            .expect("Failed to identity-map trampoline page");
        }
    })
    .expect("Frame allocator not initialized");
}
