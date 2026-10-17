//! Guarded task stacks. Virtual addresses are never recycled, so a stale
//! stack pointer cannot name a later task's stack. Backing frames are scrubbed
//! and reclaimed only after the old mapping is invalidated on every CPU.
use core::sync::atomic::{AtomicU64, Ordering};
use super::heap::with_frame_allocator;
use super::stack_layout::{BASE, LIMIT, PAGE, SIZE, STRIDE};
const _: () = assert!(SIZE == crate::scheduler::TASK_STACK_SIZE as u64);
static NEXT: AtomicU64 = AtomicU64::new(BASE + STRIDE);
static FIRST: AtomicU64 = AtomicU64::new(BASE + STRIDE);
// Keep table detachment and its shootdown indivisible with respect to new
// stack publication. In particular ARM break-before-make must finish before
// a later stack recreates a detached table covering another VA in the region.
static MAPPINGS: spin::Mutex<()> = spin::Mutex::new(());
static TASKS: [core::sync::atomic::AtomicU32; crate::scheduler::MAX_CPUS] =
    [const { core::sync::atomic::AtomicU32::new(0) }; crate::scheduler::MAX_CPUS];
pub fn set_current_task(cpu: usize, task: u32) { TASKS[cpu].store(task, Ordering::Relaxed); }

fn bottom(frame: u64) -> Option<u64> {
    super::stack_layout::bottom(frame, FIRST.load(Ordering::Acquire), NEXT.load(Ordering::Acquire))
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
    let offset = crate::random::u64().unwrap_or_else(|_| crate::random::early_u64()) & 0x3f0;
    #[cfg(feature = "test-stack-state")]
    probe_return_state(bottom(frame).unwrap(), offset);
    offset
}

#[cfg(feature = "test-stack-state")]
fn probe_return_state(bottom: u64, offset: u64) {
    use core::sync::atomic::{AtomicU8, AtomicBool};
    static TARGET: AtomicU64 = AtomicU64::new(0);
    static STAGE: AtomicU8 = AtomicU8::new(0);
    static OFFSETS: AtomicU64 = AtomicU64::new(0);
    static REPORTED: AtomicBool = AtomicBool::new(false);
    let _ = TARGET.compare_exchange(0, bottom, Ordering::AcqRel, Ordering::Acquire);
    if TARGET.load(Ordering::Acquire) == bottom {
        match STAGE.load(Ordering::Relaxed) {
            0 => {
                // This is initialized, owned stack memory, far below the
                // dispatcher's bounded frame. No uninitialized Rust reads.
                for i in 0..32 { unsafe { (bottom as *mut u8).add(i).write_volatile(0xa5); } }
                STAGE.store(1, Ordering::Release);
            }
            1 => {
                for i in 0..32 { assert_eq!(unsafe { (bottom as *const u8).add(i).read_volatile() }, 0,
                    "previous user return retained the stack test pattern"); }
                STAGE.store(2, Ordering::Release);
                crate::serial::write_line("[STACK TEST] previous user return erased stack pattern");
            }
            _ => {}
        }
    }
    let seen = OFFSETS.fetch_or(1 << (offset / 16), Ordering::Relaxed) | (1 << (offset / 16));
    if seen.count_ones() >= 2 && !REPORTED.swap(true, Ordering::Relaxed) {
        crate::serial::write_line("[STACK TEST] randomized stack offsets observed");
    }
}

/// Establish the shared top-level kernel entry before any private roots
/// copy it (x86). This permanent page is not a task stack or a guard.
pub fn init() {
    let first = BASE + ((crate::random::early_u64() & 0x1fffff) + 1) * STRIDE;
    FIRST.store(first, Ordering::Release);
    NEXT.store(first, Ordering::Release);
    with_frame_allocator(|fa| {
        let pa = fa.allocate_zeroed_frame().expect("stack region frame");
        crate::arch::stacks::map(BASE, pa, fa);
    }).expect("stack region allocator");
    #[cfg(target_arch = "aarch64")]
    crate::arch::stacks::init_emergency();
}
pub fn allocate() -> crate::scheduler::KernelStack {
    assert!(!crate::scheduler::running() || crate::arch::stacks::interrupts_enabled(), "runtime stack allocation requires IRQs enabled");
    let _mapping = MAPPINGS.lock();
    let guard = NEXT.fetch_update(Ordering::AcqRel, Ordering::Acquire,
        |old| old.checked_add(STRIDE).filter(|end| *end <= LIMIT))
        .expect("kernel stack virtual space exhausted");
    let virt = guard + PAGE;
    let phys = with_frame_allocator(|fa| {
        let mut first = None;
        assert!(!crate::arch::stacks::mapped(guard));
        assert!(!crate::arch::stacks::mapped(virt + SIZE));
        for offset in (0..SIZE).step_by(PAGE as usize) {
            let pa = fa.allocate_zeroed_frame().expect("stack frame");
            first.get_or_insert(pa);
            crate::arch::stacks::map(virt + offset, pa, fa);
        }
        first.unwrap()
    }).expect("stack allocator");
    crate::scheduler::KernelStack { virt, phys, size: SIZE }
}

/// Fatal diagnostics avoid scheduler, allocator and serial locks: overflow
/// can interrupt code holding any of them. No kernel addresses are printed.
#[no_mangle]
pub extern "C" fn kernel_stack_fatal() -> ! {
    #[cfg(target_arch = "x86_64")]
    unsafe { core::arch::asm!("cli", options(nomem, nostack)); }
    #[cfg(target_arch = "aarch64")]
    unsafe { core::arch::asm!("msr daifset, #15", options(nomem, nostack)); }
    fn write(bytes: &[u8]) { for byte in bytes { crate::arch::stacks::emergency_byte(*byte); } }
    fn decimal(mut value: u32) {
        let mut digits = [0u8; 10]; let mut at = digits.len();
        loop { at -= 1; digits[at] = b'0' + (value % 10) as u8; value /= 10; if value == 0 { break; } }
        write(&digits[at..]);
    }
    write(b"\n[STACK] DOUBLE FAULT / kernel stack guard hit on CPU");
    decimal(crate::arch::cpu_index() as u32);
    write(b" task T"); decimal(TASKS[crate::arch::cpu_index()].load(Ordering::Relaxed));
    write(if crate::arch::stacks::on_emergency_stack() { b"; emergency stack active; CPU halted\n" }
        else { b"; ERROR: outside emergency stack; CPU halted\n" });
    loop { crate::arch::halt(); }
}
pub fn release(virt: u64, phys: u64) -> u64 {
    assert!(crate::arch::stacks::interrupts_enabled(), "stack reclamation requires IRQs enabled");
    let _mapping = MAPPINGS.lock();
    assert!(virt >= BASE + 2 * PAGE && virt + SIZE < LIMIT);
    let mut pages = [0; (SIZE / PAGE) as usize];
    let retired = with_frame_allocator(|_| {
        for offset in (0..SIZE).step_by(PAGE as usize) {
            pages[(offset / PAGE) as usize] = crate::arch::stacks::unmap(virt + offset);
        }
        assert_eq!(pages[0], phys);
        crate::arch::stacks::detach_empty(virt)
    }).expect("stack allocator");
    crate::arch::stacks::invalidate();
    with_frame_allocator(|fa| {
        for pa in pages { fa.deallocate_frame(pa); }
        for pa in retired { if pa != 0 { fa.deallocate_frame(pa); } }
    }).expect("stack allocator");
    SIZE / PAGE + retired.iter().filter(|pa| **pa != 0).count() as u64
}
/// Abandon the bootstrap stack before ordinary kernel execution begins.
pub fn enter_main() -> ! {
    let stack = allocate();
    let top = stack.virt + stack.size;
    #[cfg(target_arch = "x86_64")]
    unsafe { core::arch::asm!("mov rsp, {top}", "sub rsp, 8", "mov qword ptr [rsp], 0", "jmp {entry}", "int3",
        top = in(reg) top, entry = in(reg) crate::kernel_run as *const (), options(noreturn)); }
    #[cfg(target_arch = "aarch64")]
    unsafe { core::arch::asm!("mov sp, {top}", "br {entry}", "dsb nsh", "isb",
        top = in(reg) top, entry = in(reg) crate::kernel_run as *const (), options(noreturn)); }
}
#[cfg(feature = "test-stack-overflow")]
pub fn test_overflow() -> ! {
    extern "C" fn overflow() -> ! {
        #[cfg(target_arch = "x86_64")]
        unsafe { core::arch::asm!("and rsp, -65536", "add rsp, 16", "push 0", "ud2", options(noreturn)); }
        #[cfg(target_arch = "aarch64")]
        unsafe { core::arch::asm!("mov x0, sp", "and x0, x0, #-65536", "add x0, x0, #16",
            "mov sp, x0", "str xzr, [sp]", "brk #0", options(noreturn)); }
    }
    crate::scheduler::create_task(overflow, crate::scheduler::PRIORITY_DEFAULT, "stack_overflow");
    crate::scheduler::start();
    crate::arch::enable_interrupts();
    loop { crate::arch::halt(); }
}
