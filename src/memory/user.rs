//! User address-space region: loading user-mode programs and validating user
//! pointers passed to syscalls.
//!
//! All tasks share one page table (MMURTL's flat model). User programs
//! live in a dedicated lower-half window whose pages carry the USER bit;
//! every other mapping — kernel image, heap, physical-memory window — is
//! kernel-only, so user code can't read, write, or jump into the
//! kernel. Each user task gets its own 1 MiB slot:
//!
//! ```text
//!   slot + 0x00000   unmapped (null guard)
//!   slot + 0x01000   code (read + execute)
//!   ...
//!   slot + 0xFB000   stack (read + write, no-execute), 16 KiB
//!   slot + 0xFF000   unmapped (guard above the stack)
//! ```
//!
//! Every user task has its own address space (`crate::arch::
//! new_address_space`): the kernel's mappings are shared, but the user
//! window contains only that task's own pages. Slots still give each
//! program a distinct virtual range, so a program that reaches for another
//! program's address finds nothing mapped there. Slots and spaces are
//! recycled when the task is reaped.
//!
//! Architecture-neutral: page mapping/query and the user-access window
//! (SMAP on amd64) come from `crate::arch`.

use core::sync::atomic::{AtomicU64, Ordering};

use super::heap;

/// Start of the user window (top-level table slot 200 on both amd64 and
/// arm64 — unused by the kernel, inside a 48-bit lower-half VA space)
pub const USER_BASE: u64 = 0x0000_6400_0000_0000;
/// Size of each task's slot
pub const USER_SLOT_SIZE: u64 = 0x10_0000;
/// Maximum number of user programs
pub const USER_MAX_SLOTS: u64 = 64;
/// End of the user window (exclusive)
pub const USER_END: u64 = USER_BASE + USER_SLOT_SIZE * USER_MAX_SLOTS;

const CODE_OFFSET: u64 = 0x1000;
const STACK_PAGES: u64 = 4;
const STACK_TOP_OFFSET: u64 = USER_SLOT_SIZE - 0x1000;
const PAGE: u64 = 4096;

/// Slot allocation bitmap (bit set = in use)
static SLOTS: AtomicU64 = AtomicU64::new(0);

fn alloc_slot() -> Option<u64> {
    loop {
        let cur = SLOTS.load(Ordering::Acquire);
        if cur == u64::MAX {
            return None;
        }
        let slot = (!cur).trailing_zeros() as u64;
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
    /// Its address space (page-table root)
    pub space: u64,
    /// Its slot in the user window
    pub slot: u64,
}

/// Base address of `slot` (e.g. to point a test at another program)
pub fn slot_base(slot: u64) -> u64 {
    USER_BASE + slot * USER_SLOT_SIZE
}

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
    let pa = heap::with_frame_allocator(|fa| fa.allocate_frame())
        .ok_or("frame allocator not initialized")?
        .ok_or("out of memory")?;
    let kva = crate::arch::phys_to_virt(pa);
    unsafe {
        core::ptr::write_bytes(kva, 0, PAGE as usize);
        let n = data.len().min(PAGE as usize - offset.min(PAGE as usize));
        core::ptr::copy_nonoverlapping(data.as_ptr(), kva.add(offset), n);
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
    if code.is_empty() || CODE_OFFSET + code_pages * PAGE > STACK_TOP_OFFSET - STACK_PAGES * PAGE {
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
    let base = slot_base(slot);

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
    let stack_top = base + STACK_TOP_OFFSET;
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
        stack_top,
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

/// Copy bytes in from validated user memory
pub fn copy_from_user(dst: &mut [u8], src: u64) -> bool {
    if !range_ok(src, dst.len() as u64, false) {
        return false;
    }
    crate::arch::user_access_begin();
    unsafe { core::ptr::copy_nonoverlapping(src as *const u8, dst.as_mut_ptr(), dst.len()) };
    crate::arch::user_access_end();
    true
}

/// Copy bytes out to validated, writable user memory
pub fn copy_to_user(dst: u64, src: &[u8]) -> bool {
    if !range_ok(dst, src.len() as u64, true) {
        return false;
    }
    crate::arch::user_access_begin();
    unsafe { core::ptr::copy_nonoverlapping(src.as_ptr(), dst as *mut u8, src.len()) };
    crate::arch::user_access_end();
    true
}

// ========================================================================
// ELF executables
// ========================================================================

/// ELF programs keep their stack at the top of the user window
const ELF_STACK_TOP: u64 = USER_END - PAGE;
/// ...so their segments must stay below the stack and its guard gap
const ELF_LIMIT: u64 = ELF_STACK_TOP - STACK_PAGES * PAGE - PAGE;

const PT_LOAD: u32 = 1;
const PF_X: u32 = 1;
const PF_W: u32 = 2;

fn rd16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn rd32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn rd64(b: &[u8], o: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[o..o + 8]);
    u64::from_le_bytes(a)
}

/// A PT_LOAD segment, validated
#[derive(Clone, Copy)]
struct Segment {
    vaddr: u64,
    memsz: u64,
    offset: u64,
    filesz: u64,
    writable: bool,
    executable: bool,
}

/// Check an ELF image and extract its entry point and loadable segments.
/// Everything is validated before anything is mapped: this is untrusted
/// input read from disk.
fn parse_elf(img: &[u8]) -> Result<(u64, heapless::Vec<Segment, 16>), &'static str> {
    if img.len() < 64 || &img[0..4] != b"\x7fELF" {
        return Err("not an ELF file");
    }
    if img[4] != 2 || img[5] != 1 {
        return Err("not a little-endian 64-bit ELF");
    }
    if rd16(img, 16) != 2 {
        return Err("not an executable (ET_EXEC)");
    }
    if rd16(img, 18) != crate::arch::ELF_MACHINE {
        return Err("built for a different architecture");
    }
    let entry = rd64(img, 24);
    let phoff = rd64(img, 32) as usize;
    let phentsize = rd16(img, 54) as usize;
    let phnum = rd16(img, 56) as usize;
    if phentsize < 56 || phnum == 0 || phnum > 32 {
        return Err("bad program header table");
    }
    let table_end = phoff.checked_add(phnum * phentsize).ok_or("bad program header table")?;
    if table_end > img.len() {
        return Err("program header table past end of file");
    }

    let mut segs: heapless::Vec<Segment, 16> = heapless::Vec::new();
    for i in 0..phnum {
        let ph = phoff + i * phentsize;
        if rd32(img, ph) != PT_LOAD {
            continue;
        }
        let flags = rd32(img, ph + 4);
        let seg = Segment {
            offset: rd64(img, ph + 8),
            vaddr: rd64(img, ph + 16),
            filesz: rd64(img, ph + 32),
            memsz: rd64(img, ph + 40),
            writable: flags & PF_W != 0,
            executable: flags & PF_X != 0,
        };
        if seg.memsz == 0 {
            continue;
        }
        let end = seg.vaddr.checked_add(seg.memsz).ok_or("segment wraps")?;
        if seg.vaddr < USER_BASE || end > ELF_LIMIT {
            return Err("segment outside the user window");
        }
        if seg.filesz > seg.memsz {
            return Err("segment file size exceeds memory size");
        }
        let file_end = seg.offset.checked_add(seg.filesz).ok_or("segment wraps")?;
        if file_end > img.len() as u64 {
            return Err("segment data past end of file");
        }
        if seg.writable && seg.executable {
            return Err("writable+executable segment (W^X)");
        }
        segs.push(seg).map_err(|_| "too many segments")?;
    }
    if segs.is_empty() {
        return Err("no loadable segments");
    }
    if !segs.iter().any(|s| s.executable && entry >= s.vaddr && entry < s.vaddr + s.memsz) {
        return Err("entry point not in an executable segment");
    }
    Ok((entry, segs))
}

/// Load an ELF executable into a fresh address space: each PT_LOAD
/// segment at its linked address with exactly its permissions (segments
/// may not share pages), zero-filled beyond its file data (.bss), plus a
/// stack at the top of the user window.
pub fn load_elf(img: &[u8]) -> Result<UserImage, &'static str> {
    let (entry, segs) = parse_elf(img)?;
    let slot = alloc_slot().ok_or("no free user slots")?;
    let space = match crate::arch::new_address_space() {
        Ok(s) => s,
        Err(e) => {
            free_slot(slot);
            return Err(e);
        }
    };
    let result = (|| {
        for s in &segs {
            let first = s.vaddr & !(PAGE - 1);
            let last = (s.vaddr + s.memsz + PAGE - 1) & !(PAGE - 1);
            let mut page = first;
            while page < last {
                // The slice of this segment's file data that lands on `page`
                let lo = page.max(s.vaddr);
                let hi = (page + PAGE).min(s.vaddr + s.filesz);
                let data = if hi > lo {
                    let off = (s.offset + (lo - s.vaddr)) as usize;
                    &img[off..off + (hi - lo) as usize]
                } else {
                    &[][..]
                };
                map_fresh(space, page, data, (lo - page) as usize, s.writable, s.executable)
                    .map_err(|e| if e == "page already mapped" { "segments share a page" } else { e })?;
                page += PAGE;
            }
        }
        for i in 1..=STACK_PAGES {
            map_fresh(space, ELF_STACK_TOP - i * PAGE, &[], 0, true, false)?;
        }
        Ok(())
    })();
    match result {
        Ok(()) => Ok(UserImage { entry, stack_top: ELF_STACK_TOP, space, slot }),
        Err(e) => {
            release(space, slot);
            Err(e)
        }
    }
}

/// Validate an ELF image without loading it (for tests)
pub fn check_elf(img: &[u8]) -> Result<(), &'static str> {
    parse_elf(img).map(|_| ())
}
