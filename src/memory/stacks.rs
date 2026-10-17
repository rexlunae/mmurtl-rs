//! Guarded task stacks. Virtual addresses are never recycled, so a stale
//! stack pointer cannot name a later task's stack. Backing frames are scrubbed
//! and reclaimed only after the old mapping is invalidated on every CPU.
use core::sync::atomic::{AtomicU64, Ordering};
use super::heap::with_frame_allocator;
const BASE: u64 = 0xffff_e000_0000_0000;
const LIMIT: u64 = BASE + (1 << 39);
const PAGE: u64 = 4096;
const SIZE: u64 = crate::scheduler::TASK_STACK_SIZE as u64;
static NEXT: AtomicU64 = AtomicU64::new(BASE + PAGE);

fn bottom(frame: u64) -> Option<u64> {
    if frame < BASE + 2 * PAGE || frame >= NEXT.load(Ordering::Acquire) { return None; }
    let bottom = BASE + 2 * PAGE + ((frame - BASE - PAGE) / (SIZE + 2 * PAGE)) * (SIZE + 2 * PAGE);
    (frame >= bottom && frame < bottom + SIZE).then_some(bottom)
}
/// Only assembly uses this: the returned range ends at the saved frame,
/// and is erased after this function itself has returned from that range.
#[no_mangle]
extern "C" fn kernel_stack_erase_bottom(frame: u64, user: u64) -> u64 {
    if !cfg!(feature = "stack-erase") || user == 0 { return 0; }
    bottom(frame).unwrap_or(0)
}
#[no_mangle]
extern "C" fn kernel_stack_offset(frame: u64, user: u64) -> u64 {
    if user == 0 || bottom(frame).is_none() { return 0; }
    crate::random::early_u64() & 0x3f0
}

/// Establish the shared top-level kernel entry before any private roots
/// copy it (x86). This permanent page is not a task stack or a guard.
pub fn init() {
    with_frame_allocator(|fa| {
        let pa = fa.allocate_zeroed_frame().expect("stack region frame");
        crate::arch::stacks::map(BASE, pa, fa);
    }).expect("stack region allocator");
}
pub fn allocate() -> crate::scheduler::KernelStack {
    let guard = NEXT.fetch_update(Ordering::AcqRel, Ordering::Acquire,
        |old| old.checked_add(SIZE + 2 * PAGE).filter(|end| *end <= LIMIT))
        .expect("kernel stack virtual space exhausted");
    let virt = guard + PAGE;
    let phys = with_frame_allocator(|fa| {
        let phys = fa.allocate_zeroed_contiguous((SIZE / PAGE) as usize).expect("stack frames");
        assert!(!crate::arch::stacks::mapped(guard));
        assert!(!crate::arch::stacks::mapped(virt + SIZE));
        for offset in (0..SIZE).step_by(PAGE as usize) {
            crate::arch::stacks::map(virt + offset, phys + offset, fa);
        }
        phys
    }).expect("stack allocator");
    crate::scheduler::KernelStack { virt, phys, size: SIZE }
}
pub fn release(virt: u64, phys: u64) -> u64 {
    assert!(virt >= BASE + 2 * PAGE && virt + SIZE < LIMIT);
    with_frame_allocator(|_| {
        for offset in (0..SIZE).step_by(PAGE as usize) {
            assert_eq!(crate::arch::stacks::unmap(virt + offset), phys + offset);
        }
    }).expect("stack allocator");
    crate::arch::stacks::invalidate();
    with_frame_allocator(|fa| {
        for offset in (0..SIZE).step_by(PAGE as usize) { fa.deallocate_frame(phys + offset); }
    }).expect("stack allocator");
    SIZE / PAGE
}
