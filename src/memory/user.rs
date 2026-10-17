//! User address-space region: loading user-mode programs and validating user
//! pointers passed to syscalls.
//!
//! Each task has a private user window in one lower-half 512 GiB table
//! slot. Kernel mappings are shared and supervisor-only. Flat PIC and PIE
//! images choose a fresh page-aligned code bias in the lower half; the
//! guarded NX stack is placed independently in the upper half. A randomized
//! aligned initial SP leaves room for the ELF startup block above it.
//!
//! The 64-bit ownership bitmap limits live images independently of their
//! randomized virtual addresses. Reaping releases their private mappings
//! and ownership IDs. Legacy ET_EXEC code keeps its linked address, while
//! its stack still receives a fresh placement.
//!
//! Architecture-neutral: page mapping/query and the user-access window
//! (SMAP on amd64) come from `crate::arch`.

use core::sync::atomic::{AtomicU64, Ordering};

use super::heap;

/// Start of the user window (top-level table slot 200 on both amd64 and
/// arm64 — unused by the kernel, inside a 48-bit lower-half VA space)
pub const USER_BASE: u64 = super::user_layout::BASE;
/// Maximum number of user programs
pub const USER_MAX_SLOTS: u64 = 64;
/// End of the user window (exclusive)
pub const USER_END: u64 = super::user_layout::END;

const CODE_OFFSET: u64 = 0x1000;
const STACK_PAGES: u64 = 4;
const MAX_FLAT_IMAGE: u64 = (1 << 20) - 6 * PAGE;
const PAGE: u64 = 4096;

/// Slot allocation bitmap (bit set = in use)
static SLOTS: AtomicU64 = AtomicU64::new(0);

fn alloc_slot() -> Option<u64> {
    let rotate = random_word() as u32 & 63;
    loop {
        let cur = SLOTS.load(Ordering::Acquire);
        if cur == u64::MAX {
            return None;
        }
        let slot = (((!cur).rotate_right(rotate).trailing_zeros() + rotate) & 63) as u64;
        if SLOTS
            .compare_exchange(cur, cur | 1 << slot, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
        {
            return Some(slot);
        }
    }
}

fn free_slot(slot: u64) {
    SLOTS.fetch_and(!(1 << slot), Ordering::AcqRel);
}

/// A loaded user program
pub struct UserImage {
    /// Entry point (first byte of the code)
    pub entry: u64,
    /// Initial user stack pointer
    pub stack_top: u64,
    /// Top mapped stack edge, independent of the randomized initial SP.
    pub stack_edge: u64,
    /// ELF startup block pointer (flat binaries retain their existing arg ABI).
    pub startup: u64,
    /// Its address space (page-table root)
    pub space: u64,
    /// Its ownership ID (independent of virtual placement)
    pub slot: u64,
}
fn random_word() -> u64 {
    static WARNED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
    crate::random::u64().unwrap_or_else(|_| {
        if !WARNED.swap(true,Ordering::Relaxed) {
            crate::serial::write_line("[USER] WARNING: ASLR/canary entropy unavailable; placement is not cryptographically assured");
        }
        crate::random::early_u64()
    })
}
fn layout() -> super::user_layout::Layout { super::user_layout::choose(random_word(),random_word(),random_word()) }

/// Check the user window is free before first use
pub fn init() -> bool {
    use core::fmt::Write;
    let free = crate::arch::query_page(USER_BASE).is_none();
    // One write, so the line can't interleave with tasks already running
    let mut line: heapless::String<96> = heapless::String::new();
    let _ = write!(
        line,
        "[USER] User window 0x{:x}..0x{:x} {}\n",
        USER_BASE,
        USER_END,
        if free { "ready" } else { "ALREADY MAPPED — userspace disabled" }
    );
    crate::serial::write_str(&line);
    free
}

/// Map one fresh, zeroed frame at `va` in address space `space`, copying
/// `data` into it at byte `offset` first.
pub fn map_fresh(
    space: u64,
    va: u64,
    data: &[u8],
    offset: usize,
    writable: bool,
    executable: bool,
) -> Result<(), &'static str> {
    let pa = heap::with_frame_allocator(|fa| fa.allocate_zeroed_frame())
        .ok_or("frame allocator not initialized")?
        .ok_or("out of memory")?;
    let kva = crate::arch::phys_to_virt(pa);
    unsafe {
        let n = data.len().min(PAGE as usize - offset.min(PAGE as usize));
        core::ptr::copy_nonoverlapping(data.as_ptr(), kva.add(offset), n);
    }
    if executable {
        crate::arch::sync_icache(kva, PAGE as usize);
    }
    crate::arch::map_user_page(space, va, pa, writable, executable).map_err(|e| {
        heap::with_frame_allocator(|fa| fa.deallocate_frame(pa));
        e
    })
}

/// Release a program's address space and slot; returns frames freed.
/// The space must no longer be loaded on any CPU (see the reaper).
pub fn release(space: u64, slot: u64) -> usize {
    let freed = crate::arch::free_address_space(space);
    free_slot(slot);
    freed
}

/// Load a position-independent flat binary into a fresh address space
/// and slot: code pages read-only + executable, a writable
/// non-executable stack.
pub fn load_program(code: &[u8]) -> Result<UserImage, &'static str> {
    let code_pages = (code.len() as u64 + PAGE - 1) / PAGE;
    if code.is_empty() || code_pages * PAGE > MAX_FLAT_IMAGE {
        return Err("program too large for a user slot");
    }
    let slot = alloc_slot().ok_or("no free user slots")?;
    let space = match crate::arch::new_address_space() {
        Ok(s) => s,
        Err(e) => {
            free_slot(slot);
            return Err(e);
        }
    };
    match populate(space, slot, code, code_pages) {
        Ok(img) => Ok(img),
        Err(e) => {
            release(space, slot);
            Err(e)
        }
    }
}

fn populate(space: u64, slot: u64, code: &[u8], code_pages: u64) -> Result<UserImage, &'static str> {
    let layout = layout();
    let base = layout.bias;

    for i in 0..code_pages {
        let start = (i * PAGE) as usize;
        let end = (start + PAGE as usize).min(code.len());
        map_fresh(
            space,
            base + CODE_OFFSET + i * PAGE,
            &code[start..end],
            0,
            false,
            true,
        )?;
    }
    let stack_top = layout.stack_edge;
    for i in 1..=STACK_PAGES {
        map_fresh(
            space,
            stack_top - i * PAGE,
            &[],
            0,
            true,
            false,
        )?;
    }

    Ok(UserImage {
        entry: base + CODE_OFFSET,
        stack_top: layout.stack_pointer,
        stack_edge: stack_top,
        startup: 0,
        space,
        slot,
    })
}

/// Whether `[ptr, ptr+len)` lies entirely in user-accessible memory
/// (writable too, if `write`). A syscall must pass this before the kernel
/// touches a user pointer — otherwise user code could make the kernel read or
/// write kernel memory on its behalf.
pub fn range_ok(ptr: u64, len: u64, write: bool) -> bool {
    if len == 0 {
        return true;
    }
    let end = match ptr.checked_add(len) {
        Some(e) => e,
        None => return false,
    };
    if ptr < USER_BASE || end > USER_END {
        return false;
    }
    let ptr = mask_user_ptr(ptr, len);
    crate::arch::nospec::barrier_nospec();
    let mut page = ptr & !(PAGE - 1);
    while page < end {
        match crate::arch::query_page(page) {
            Some((true, writable)) => {
                if write && !writable {
                    return false;
                }
            }
            _ => return false,
        }
        page += PAGE;
    }
    true
}

/// Cheap overflow-safe bounds check. Hardware enforces page permissions
/// during the fault-tolerant copy; no check-then-use page walk is needed.
pub fn bounds_ok(ptr: u64, len: u64) -> bool {
    super::user_bounds::bounds_ok(ptr, len, USER_BASE, USER_END)
}

/// Mask invalid pointers to the permanently unmapped end of the user
/// window. Opaque assembly comparisons preserve a data dependency even
/// when a preceding bounds-check branch is mistrained.
#[inline(always)]
pub fn mask_user_ptr(ptr: u64, len: u64) -> u64 {
    super::user_bounds::mask_user_ptr(ptr, len, USER_BASE, USER_END)
}

/// On failure, the destination may contain a copied prefix. Callers must
/// discard it (RQBs are decoded only after this returns true).
#[inline(never)]
pub fn copy_from_user(dst: &mut [u8], src: u64) -> bool {
    if !bounds_ok(src, dst.len() as u64) { return false; }
    let src = mask_user_ptr(src, dst.len() as u64);
    unsafe { crate::arch::uaccess::copy_from_user(dst.as_mut_ptr(), src, dst.len()) == 0 }
}

/// Failure may have written a prefix of the user buffer.
#[inline(never)]
pub fn copy_to_user(dst: u64, src: &[u8]) -> bool {
    if !bounds_ok(dst, src.len() as u64) { return false; }
    let dst = mask_user_ptr(dst, src.len() as u64);
    unsafe { crate::arch::uaccess::copy_to_user(dst, src.as_ptr(), src.len()) == 0 }
}

// ========================================================================
// ELF executables
// ========================================================================

/// Load a validated ET_EXEC or static PIE with RELATIVE relocations and RELRO.
pub fn load_elf(bytes: &[u8]) -> Result<UserImage, &'static str> {
    let image=super::elf::parse(bytes,crate::arch::ELF_MACHINE,USER_BASE,super::user_layout::IMAGE_LIMIT)?;
    let placement=layout();
    let bias=if image.pie { placement.image_bias(image.alignment) } else { 0 };
    let slot=alloc_slot().ok_or("no free user slots")?;
    let space=match crate::arch::new_address_space() { Ok(space)=>space,Err(e)=>{free_slot(slot);return Err(e);} };
    let startup=placement.stack_edge-32;
    let result=(|| {
        for segment in &image.segments {
            let first=segment.vaddr&!(PAGE-1);
            let last=(segment.vaddr+segment.memsz+PAGE-1)&!(PAGE-1);
            for page in (first..last).step_by(PAGE as usize) {
                let low=page.max(segment.vaddr);
                let high=(page+PAGE).min(segment.vaddr+segment.filesz);
                let data=if high>low { let offset=(segment.offset+low-segment.vaddr) as usize; &bytes[offset..offset+(high-low) as usize] } else { &[] };
                let pa=heap::with_frame_allocator(|fa|fa.allocate_zeroed_frame()).flatten().ok_or("out of memory")?;
                let kva=crate::arch::phys_to_virt(pa);
                unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(),kva.add((low-page) as usize),data.len()); }
                for (target,addend) in image.relocations(bytes) {
                    if target>=page && target<page+PAGE {
                        // Both bias and nonnegative addend are bounded by validation/layout.
                        let value=bias+addend as u64;
                        unsafe { kva.add((target-page) as usize).cast::<u64>().write_unaligned(value); }
                    }
                }
                if segment.executable { crate::arch::sync_icache(kva,PAGE as usize); }
                let writable=segment.writable && !image.readonly_page(page);
                if let Err(error)=crate::arch::map_user_page(space,bias+page,pa,writable,segment.executable) {
                    heap::with_frame_allocator(|fa|fa.deallocate_frame(pa)); return Err(error);
                }
            }
        }
        let guard=(random_word() & !255).max(256);
        let mut block=[0u8;32];
        block[..8].copy_from_slice(&super::user_layout::STARTUP_MAGIC.to_le_bytes());
        block[8..16].copy_from_slice(&guard.to_le_bytes());
        for i in 1..=STACK_PAGES {
            map_fresh(space,placement.stack_edge-i*PAGE,if i==1 {&block} else {&[]},if i==1 {(PAGE-32) as usize} else {0},true,false)?;
        }
        Ok(UserImage {entry:bias+image.entry,stack_top:placement.stack_pointer,stack_edge:placement.stack_edge,startup,space,slot})
    })();
    if result.is_err() { release(space,slot); }
    result
}
/// Validate without allocating frames or accepting untrusted relocations.
pub fn check_elf(bytes: &[u8]) -> Result<(), &'static str> {
    super::elf::parse(bytes,crate::arch::ELF_MACHINE,USER_BASE,super::user_layout::IMAGE_LIMIT).map(|_|())
}

/// Deterministic check-then-unmap test, before any user tasks are started.
/// The private space is never runnable on another CPU. No scheduler or
/// frame lock is held across a copy that deliberately takes a fault.
pub fn uaccess_self_test() {
    let img = match load_program(&[0x5a]) {
        Ok(img) => img,
        Err(error) => {
            crate::serial::write_str("[TEST] user-copy fixups skipped: ");
            crate::serial::write_line(error);
            return;
        }
    };
    let mut pass = true;
    for (ptr, len, expected) in [(USER_BASE, 1, USER_BASE), (USER_END - 1, 1, USER_END - 1),
        (USER_BASE - 1, 1, USER_END), (USER_END - 1, 2, USER_END),
        (u64::MAX, 2, USER_END), (USER_BASE, u64::MAX, USER_END)] {
        pass &= mask_user_ptr(ptr, len) == expected;
    }
    let mut removed = 0;
    crate::arch::without_interrupts(|| {
        let cpu = crate::arch::cpu_index();
        crate::arch::switch_address_space(cpu, img.space);
        let mut byte = [0u8; 1];
        pass &= copy_from_user(&mut byte, img.entry) && byte == [0x5a];
        pass &= !copy_to_user(img.entry, &[1]); // read-only code page
        let boundary = img.stack_edge - 8;
        pass &= copy_to_user(boundary, &[0x33; 8]);
        let mut partial = [0u8; 16];
        pass &= !copy_from_user(&mut partial, boundary); // prefix then guard fault
        pass &= partial[..8] == [0x33; 8];
        pass &= !copy_to_user(boundary, &[0x44; 16]);
        let page = img.stack_edge - PAGE;
        pass &= range_ok(page, 8, true);
        removed = unsafe { crate::arch::uaccess::remove_test_page(img.space, page) };
        pass &= !copy_from_user(&mut byte, page); // stale validation, actual fault
        pass &= !copy_to_user(page, &[1]);
        pass &= !copy_from_user(&mut byte, u64::MAX);
        pass &= !copy_to_user(USER_END, &[1]);
        crate::arch::switch_address_space(cpu, 0);
    });
    heap::with_frame_allocator(|fa| fa.deallocate_frame(removed));
    release(img.space, img.slot);
    assert!(pass, "fault-tolerant user-copy checks failed");
    crate::serial::write_line("[TEST] user-copy fixups: read-only, guard, stale validation recovered");
}
