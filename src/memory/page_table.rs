//! Page Table Management — Virtual memory mapping for x86_64 long mode.
//!
//! In x86_64 long mode, we use 4-level paging: PML4 → PDP → PD → PT → 4K page.
//! Each table has 512 entries covering a 512 GiB region at each level.
//!
//! The bootloader sets up initial page tables identity-mapping the kernel and
//! providing a physical memory offset. This module extends that with functions
//! to map and unmap pages on demand.

use x86_64::{
    PhysAddr, VirtAddr,
    structures::paging::{
        PageTable, Page,
        page_table::PageTableFlags as Flags,
        FrameAllocator,
        PhysFrame,
    },
};

use crate::memory::frame_allocator::BumpFrameAllocator;

/// Global physical memory offset — set once during init
pub static mut PHYSICAL_MEMORY_OFFSET: u64 = 0;

/// Safe getter for the physical memory offset
pub fn physical_memory_offset() -> u64 {
    unsafe { PHYSICAL_MEMORY_OFFSET }
}

/// Initialize page table management from boot info
pub fn init(
    physical_memory_offset: Option<u64>,
    _boot_info: &bootloader_api::BootInfo,
) {
    if let Some(offset) = physical_memory_offset {
        crate::serial::write_str("[PAGING] Physical memory offset: 0x");
        crate::serial::write_hex(offset);
        crate::serial::write_str("\n");

        unsafe {
            PHYSICAL_MEMORY_OFFSET = offset;
        }
    }
}

/// Convert a physical address to a virtual address for kernel access
fn phys_to_virt(phys: PhysAddr) -> VirtAddr {
    VirtAddr::new(phys.as_u64() + unsafe { PHYSICAL_MEMORY_OFFSET })
}

/// Get a reference to the active PML4 (level 4) page table
///
/// The bootloader maps all physical memory at the physical memory offset,
/// so we can access any page table entry by treating it as a data structure
/// in that virtual address range.
pub unsafe fn active_pml4() -> &'static mut PageTable {
    use x86_64::registers::control::Cr3;
    let (pml4_frame, _) = Cr3::read();
    let virt = phys_to_virt(pml4_frame.start_address());
    &mut *virt.as_mut_ptr::<PageTable>()
}

/// Translate a virtual address to a physical address by walking the page tables
pub fn translate_virtual(vaddr: VirtAddr) -> Option<PhysAddr> {
    unsafe {
        let pml4 = active_pml4();

        // PML4 index (bits 39-47)
        let pml4e = &pml4[vaddr.p4_index()];
        if !pml4e.flags().contains(Flags::PRESENT) {
            return None;
        }
        let pdpt_phys = pml4e.addr();
        let pdpt = &*phys_to_virt(pdpt_phys).as_ptr::<PageTable>();

        // PDP index (bits 30-38)
        let pdpte = &pdpt[vaddr.p3_index()];
        if !pdpte.flags().contains(Flags::PRESENT) {
            return None;
        }
        if pdpte.flags().contains(Flags::HUGE_PAGE) {
            return Some(PhysAddr::new(
                pdpte.addr().as_u64() + (vaddr.as_u64() & (512 * 1024 * 1024 - 1))
            ));
        }
        let pd_phys = pdpte.addr();
        let pd = &*phys_to_virt(pd_phys).as_ptr::<PageTable>();

        // PD index (bits 21-29)
        let pde = &pd[vaddr.p2_index()];
        if !pde.flags().contains(Flags::PRESENT) {
            return None;
        }
        if pde.flags().contains(Flags::HUGE_PAGE) {
            return Some(PhysAddr::new(
                pde.addr().as_u64() + (vaddr.as_u64() & (2 * 1024 * 1024 - 1))
            ));
        }
        let pt_phys = pde.addr();
        let pt = &*phys_to_virt(pt_phys).as_ptr::<PageTable>();

        // PT index (bits 12-20)
        let pte = &pt[vaddr.p1_index()];
        if !pte.flags().contains(Flags::PRESENT) {
            return None;
        }
        Some(PhysAddr::new(
            pte.addr().as_u64() + (vaddr.as_u64() & 0xFFF)
        ))
    }
}

/// Map a single 4 KiB page
///
/// Walks the page tables, creating intermediate tables as needed.
/// The new table entries are allocated from the frame allocator.
///
/// # Safety
/// Caller must ensure the virtual address range is not already mapped.
pub unsafe fn map_page(
    page: Page,
    phys_frame: PhysFrame,
    flags: Flags,
    frame_allocator: &mut BumpFrameAllocator,
) -> Result<(), &'static str> {
    let pml4 = active_pml4();

    // Level 3: PDP (Page Directory Pointer Table)
    let pml4e = &mut pml4[page.p4_index()];
    let pdpt: &mut PageTable = if !pml4e.flags().contains(Flags::PRESENT) {
        let new_frame = frame_allocator.allocate_frame().ok_or("OOM: PDP table")?;
        let pdpt_virt = phys_to_virt(new_frame.start_address());
        let pdpt = &mut *pdpt_virt.as_mut_ptr::<PageTable>();
        pdpt.zero();
        pml4e.set_addr(new_frame.start_address(), Flags::PRESENT | Flags::WRITABLE | Flags::ACCESSED);
        pdpt
    } else {
        &mut *phys_to_virt(pml4e.addr()).as_mut_ptr::<PageTable>()
    };

    // Level 2: PD (Page Directory)
    let pdpe = &mut pdpt[page.p3_index()];
    let pd: &mut PageTable = if !pdpe.flags().contains(Flags::PRESENT) {
        let new_frame = frame_allocator.allocate_frame().ok_or("OOM: PD table")?;
        let pd_virt = phys_to_virt(new_frame.start_address());
        let pd = &mut *pd_virt.as_mut_ptr::<PageTable>();
        pd.zero();
        pdpe.set_addr(new_frame.start_address(), Flags::PRESENT | Flags::WRITABLE | Flags::ACCESSED);
        pd
    } else {
        &mut *phys_to_virt(pdpe.addr()).as_mut_ptr::<PageTable>()
    };

    // Level 1: PT (Page Table)
    let pde = &mut pd[page.p2_index()];
    let pt: &mut PageTable = if !pde.flags().contains(Flags::PRESENT) {
        let new_frame = frame_allocator.allocate_frame().ok_or("OOM: PT table")?;
        let pt_virt = phys_to_virt(new_frame.start_address());
        let pt = &mut *pt_virt.as_mut_ptr::<PageTable>();
        pt.zero();
        pde.set_addr(new_frame.start_address(), Flags::PRESENT | Flags::WRITABLE | Flags::ACCESSED);
        pt
    } else {
        &mut *phys_to_virt(pde.addr()).as_mut_ptr::<PageTable>()
    };

    // Set the final page table entry
    let pte = &mut pt[page.p1_index()];
    if pte.flags().contains(Flags::PRESENT) {
        return Err("Page already mapped");
    }
    pte.set_addr(phys_frame.start_address(), flags | Flags::PRESENT);

    // Flush TLB for this page
    x86_64::instructions::tlb::flush(page.start_address());

    Ok(())
}

/// Unmap a single 4 KiB page (manual implementation)
pub unsafe fn unmap_page(page: Page) -> Result<PhysFrame, &'static str> {
    let pml4 = active_pml4();

    let pml4e = &pml4[page.p4_index()];
    if !pml4e.flags().contains(Flags::PRESENT) {
        return Err("PML4E not present");
    }
    let pdpt = &mut *phys_to_virt(pml4e.addr()).as_mut_ptr::<PageTable>();
    let pdpte = &pdpt[page.p3_index()];
    if !pdpte.flags().contains(Flags::PRESENT) {
        return Err("PDPTE not present");
    }
    let pd = &mut *phys_to_virt(pdpte.addr()).as_mut_ptr::<PageTable>();
    let pde = &pd[page.p2_index()];
    if !pde.flags().contains(Flags::PRESENT) {
        return Err("PDE not present");
    }
    let pt = &mut *phys_to_virt(pde.addr()).as_mut_ptr::<PageTable>();
    let pte = &mut pt[page.p1_index()];
    if !pte.flags().contains(Flags::PRESENT) {
        return Err("PTE not present");
    }

    // Get the frame before clearing
    let frame = PhysFrame::from_start_address(pte.addr())
        .map_err(|_| "Unaligned frame address")?;

    // Clear the entry
    pte.set_flags(Flags::empty());
    pte.set_addr(PhysAddr::new(0), Flags::empty());

    // Flush TLB
    x86_64::instructions::tlb::flush(page.start_address());

    Ok(frame)
}

/// Get page table flags for a mapped page
pub fn query_page(vaddr: VirtAddr) -> Option<Flags> {
    unsafe {
        let pml4 = active_pml4();
        let pml4e = &pml4[vaddr.p4_index()];
        if !pml4e.flags().contains(Flags::PRESENT) { return None; }

        let pdpt = &*phys_to_virt(pml4e.addr()).as_ptr::<PageTable>();
        let pdpte = &pdpt[vaddr.p3_index()];
        if !pdpte.flags().contains(Flags::PRESENT) { return None; }
        if pdpte.flags().contains(Flags::HUGE_PAGE) {
            return Some(pdpte.flags() - Flags::HUGE_PAGE);
        }

        let pd = &*phys_to_virt(pdpte.addr()).as_ptr::<PageTable>();
        let pde = &pd[vaddr.p2_index()];
        if !pde.flags().contains(Flags::PRESENT) { return None; }
        if pde.flags().contains(Flags::HUGE_PAGE) {
            return Some(pde.flags() - Flags::HUGE_PAGE);
        }

        let pt = &*phys_to_virt(pde.addr()).as_ptr::<PageTable>();
        let pte = &pt[vaddr.p1_index()];
        if !pte.flags().contains(Flags::PRESENT) { return None; }
        Some(pte.flags())
    }
}
