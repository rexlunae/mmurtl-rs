//! User address-space region: loading ring-3 programs and validating user
//! pointers passed to syscalls.
//!
//! All tasks share one page table (MMURTL's flat model). User programs
//! live in a dedicated lower-half window whose pages carry the USER bit;
//! every other mapping — kernel image, heap, physical-memory window — is
//! supervisor-only, so ring-3 code can't read, write, or jump into the
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
//! Slots are not yet isolated from each other (one shared address space);
//! per-task page tables are the natural next step.
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

static NEXT_SLOT: AtomicU64 = AtomicU64::new(0);

/// A loaded user program
pub struct UserImage {
    /// Entry point (first byte of the code)
    pub entry: u64,
    /// Initial user stack pointer
    pub stack_top: u64,
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

/// Map one fresh, zeroed frame at `va`, optionally copying `data` into
/// it first.
fn map_fresh(va: u64, data: &[u8], writable: bool, executable: bool) -> Result<(), &'static str> {
    let pa = heap::with_frame_allocator(|fa| fa.allocate_frame())
        .ok_or("frame allocator not initialized")?
        .ok_or("out of memory")?;
    let kva = crate::arch::phys_to_virt(pa);
    unsafe {
        core::ptr::write_bytes(kva, 0, PAGE as usize);
        core::ptr::copy_nonoverlapping(data.as_ptr(), kva, data.len().min(PAGE as usize));
    }
    crate::arch::map_user_page(va, pa, writable, executable)
}

/// Load a position-independent flat binary into a fresh slot: code pages
/// read-only + executable, a writable non-executable stack.
pub fn load_program(code: &[u8]) -> Result<UserImage, &'static str> {
    let code_pages = (code.len() as u64 + PAGE - 1) / PAGE;
    if code.is_empty() || CODE_OFFSET + code_pages * PAGE > STACK_TOP_OFFSET - STACK_PAGES * PAGE {
        return Err("program too large for a user slot");
    }
    let slot = NEXT_SLOT.fetch_add(1, Ordering::Relaxed);
    if slot >= USER_MAX_SLOTS {
        return Err("no free user slots");
    }
    let base = USER_BASE + slot * USER_SLOT_SIZE;

    for i in 0..code_pages {
        let start = (i * PAGE) as usize;
        let end = (start + PAGE as usize).min(code.len());
        map_fresh(
            base + CODE_OFFSET + i * PAGE,
            &code[start..end],
            false,
            true,
        )?;
    }
    let stack_top = base + STACK_TOP_OFFSET;
    for i in 1..=STACK_PAGES {
        map_fresh(
            stack_top - i * PAGE,
            &[],
            true,
            false,
        )?;
    }

    Ok(UserImage {
        entry: base + CODE_OFFSET,
        stack_top,
    })
}

/// Whether `[ptr, ptr+len)` lies entirely in user-accessible memory
/// (writable too, if `write`). A syscall must pass this before the kernel
/// touches a user pointer — otherwise ring 3 could make the kernel read or
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
