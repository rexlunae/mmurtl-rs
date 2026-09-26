//! Scheduler — SMP preemptive round-robin task scheduler with RQB IPC.
//!
//! Every online CPU runs the scheduler: each CPU's LAPIC timer fires at
//! ~100 Hz (PIT+PIC fallback on the BSP when no APIC exists) and enters
//! `schedule_and_switch`. Tasks live in one global run queue protected by a
//! spinlock; any CPU may pick up any unpinned Ready task, so tasks migrate
//! freely between cores. Each CPU has an "idle task" — its own boot/park
//! HLT loop, adopted into the task list — that it falls back to when no
//! normal-priority work is ready.
//!
//! Locking rules:
//!   - The scheduler lock is only taken with interrupts disabled on the
//!     taking CPU (interrupt handlers run with IF=0 already).
//!   - Nothing prints to serial while holding the scheduler lock — a
//!     preempted serial writer on another CPU would deadlock us.
//!   - During a context switch the lock is held *across* the stack switch
//!     (released by `scheduler_unlock` from the timer asm) so a task's
//!     saved context can't be resumed by another CPU while its old stack
//!     is still in use.

mod task;
mod rqb;

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

pub use task::*;
pub use rqb::*;

// ========================================================================
// Scheduler Constants
// ========================================================================

/// Default stack size for each task (32 KiB)
pub const TASK_STACK_SIZE: usize = 32 * 1024;

/// Maximum number of tasks
pub const MAX_TASKS: usize = 64;

/// Maximum number of CPUs the scheduler tracks
pub const MAX_CPUS: usize = 64;

/// Timer tick frequency per CPU (Hz)
pub const SCHEDULER_FREQUENCY_HZ: u32 = 100;

/// Maximum queued (unreceived) requests per task
pub const INBOX_CAPACITY: usize = 32;

/// System clock: ticks of CPU 0's timer since boot (one per
/// 1/SCHEDULER_FREQUENCY_HZ s). Drives `sleep_ms`.
static JIFFIES: AtomicU64 = AtomicU64::new(0);

/// Current system time in ticks
pub fn jiffies() -> u64 {
    JIFFIES.load(Ordering::Relaxed)
}

// ========================================================================
// Per-CPU state
// ========================================================================

#[derive(Clone, Copy)]
struct PerCpu {
    /// Whether this CPU has registered with the scheduler
    registered: bool,
    /// This CPU's hardware ID (Local APIC ID / MPIDR affinity), the
    /// target for reschedule IPIs
    hw_id: u32,
    /// Index of the task currently running on this CPU
    current: Option<usize>,
    /// Index of this CPU's pinned idle task
    idle_idx: usize,
}

impl PerCpu {
    const EMPTY: Self = Self {
        registered: false,
        hw_id: 0,
        current: None,
        idle_idx: 0,
    };
}

/// The CPU index of the calling processor
pub fn current_cpu() -> usize {
    crate::arch::cpu_index()
}

/// Per-CPU: this CPU took the scheduler lock in an interrupt/yield path
/// and still holds it across the stack switch (see `scheduler_unlock`)
static SWITCH_LOCK_HELD: [AtomicBool; MAX_CPUS] = {
    const F: AtomicBool = AtomicBool::new(false);
    [F; MAX_CPUS]
};

// ========================================================================
// Scheduler State
// ========================================================================

/// The global scheduler
pub struct Scheduler {
    /// All registered tasks (run queue)
    tasks: Vec<Box<TaskControlBlock>>,
    /// Per-CPU scheduling state
    cpus: [PerCpu; MAX_CPUS],
    /// Global round-robin cursor — spreads tasks across CPUs
    rr_cursor: usize,
    /// Number of ticks since scheduler start (all CPUs)
    tick_count: u64,
    /// Whether the scheduler is initialized
    initialized: AtomicBool,
}

impl Scheduler {
    const fn new() -> Self {
        Self {
            tasks: Vec::new(),
            cpus: [PerCpu::EMPTY; MAX_CPUS],
            rr_cursor: 0,
            tick_count: 0,
            initialized: AtomicBool::new(false),
        }
    }

    /// Register a CPU with the scheduler, adopting its current execution
    /// context (the boot/park HLT loop) as that CPU's pinned idle task.
    fn register_cpu(&mut self, cpu: usize, hw_id: u32) -> u32 {
        let idle = TaskControlBlock::adopt_current("idle", PRIORITY_IDLE, cpu as u8);
        let tid = idle.id;
        self.tasks.push(idle);
        let idx = self.tasks.len() - 1;

        self.cpus[cpu] = PerCpu {
            registered: true,
            hw_id,
            current: Some(idx),
            idle_idx: idx,
        };
        tid
    }

    /// Add a prepared task to the run queue
    fn add_task(&mut self, task: Box<TaskControlBlock>) -> u32 {
        let tid = task.id;
        self.tasks.push(task);
        tid
    }

    /// Called on each timer tick, reschedule IPI, or voluntary yield on
    /// any CPU.
    ///
    /// # Safety
    /// Only called from interrupt context with the scheduler lock held.
    /// Takes the current RSP (pointing to saved TaskContext) and returns
    /// the next task's context pointer as the new RSP.
    pub unsafe fn on_tick(&mut self, cpu: usize, current_rsp: u64, timer: bool) -> u64 {
        if !self.cpus[cpu].registered || self.tasks.is_empty() {
            return current_rsp;
        }

        if timer {
            self.tick_count += 1;
            if cpu == 0 {
                JIFFIES.fetch_add(1, Ordering::Relaxed);
            }
        }

        // Wake sleepers whose deadline has passed. This CPU takes one of
        // them; kick idle CPUs for the rest so wakeups spread across cores
        // instead of all landing on CPU 0 (whose tick advances the clock).
        let now = JIFFIES.load(Ordering::Relaxed);
        let mut woken = 0usize;
        for t in self.tasks.iter_mut() {
            if t.state == TaskState::Sleeping && t.wake_tick <= now {
                t.state = TaskState::Ready;
                woken += 1;
            }
        }
        if woken > 1 && crate::arch::ipi_available() {
            for (i, c) in self.cpus.iter().enumerate() {
                if woken <= 1 {
                    break;
                }
                if c.registered && i != cpu && c.current == Some(c.idle_idx) {
                    crate::arch::send_resched_ipi(c.hw_id);
                    woken -= 1;
                }
            }
        }

        // Save the interrupted context into the current task
        if let Some(cur) = self.cpus[cpu].current {
            let t = &mut self.tasks[cur];
            t.context_ptr = current_rsp;
            t.on_cpu = None;
            if timer {
                t.total_ticks += 1;
            }
            if t.state == TaskState::Running {
                t.state = TaskState::Ready;
            }
        }

        // Pick the next task for this CPU
        let next = self.pick_next(cpu);
        let t = &mut self.tasks[next];
        t.state = TaskState::Running;
        t.on_cpu = Some(cpu as u8);
        if t.user {
            // Traps from user mode must land on this task's own kernel stack
            crate::arch::on_switch_to_user(cpu, t.kernel_stack_top);
        }
        self.cpus[cpu].current = Some(next);
        self.tasks[next].context_ptr
    }

    /// Pick the next task for `cpu`: round-robin over Ready, unpinned (or
    /// pinned-here), non-idle tasks that are not still executing on some
    /// other CPU; fall back to this CPU's idle task.
    fn pick_next(&mut self, cpu: usize) -> usize {
        let n = self.tasks.len();
        for offset in 1..=n {
            let idx = (self.rr_cursor + offset) % n;
            let t = &self.tasks[idx];
            if t.state != TaskState::Ready || t.on_cpu.is_some() {
                continue;
            }
            if t.priority == PRIORITY_IDLE {
                continue;
            }
            if let Some(p) = t.pinned_cpu {
                if p as usize != cpu {
                    continue;
                }
            }
            self.rr_cursor = idx;
            return idx;
        }

        // Nothing runnable — this CPU idles
        self.cpus[cpu].idle_idx
    }

    /// Find a CPU (≠ `exclude`) currently running its idle task, for a
    /// reschedule IPI. Returns its APIC ID.
    fn find_idle_cpu(&self, exclude: usize) -> Option<u32> {
        for (i, c) in self.cpus.iter().enumerate() {
            if c.registered && i != exclude && c.current == Some(c.idle_idx) {
                return Some(c.hw_id);
            }
        }
        None
    }

    /// Get the current task ID on the calling CPU
    fn current_id(&self, cpu: usize) -> u32 {
        match self.cpus[cpu].current {
            Some(idx) => self.tasks[idx].id,
            None => 0,
        }
    }

    /// Index of the task running on `cpu`
    fn current_idx(&self, cpu: usize) -> usize {
        self.cpus[cpu].current.expect("CPU has no current task")
    }

    /// Index of a live (non-exited) task by ID
    fn find_live(&self, tid: u32) -> Option<usize> {
        self.tasks
            .iter()
            .position(|t| t.id == tid && t.state != TaskState::Exited)
    }

    /// Queue `msg` for `receiver` and block the caller in WaitingReply.
    /// Returns a CPU to kick if the receiver was woken.
    fn post_request(&mut self, cpu: usize, receiver: u32, msg: &Rqb) -> Result<Option<u32>, RqbStatus> {
        let me = self.current_idx(cpu);
        let my_id = self.tasks[me].id;
        if receiver == my_id {
            return Err(RqbStatus::InvalidParam); // would deadlock on itself
        }
        let ri = self.find_live(receiver).ok_or(RqbStatus::NotFound)?;
        if self.tasks[ri].inbox.len() >= INBOX_CAPACITY {
            return Err(RqbStatus::Busy);
        }

        let mut m = msg.clone();
        m.sender_id = my_id;
        m.receiver_id = receiver;
        self.tasks[ri].inbox.push_back(m);

        let t = &mut self.tasks[me];
        t.state = TaskState::WaitingReply;
        t.wait_for = receiver;
        t.reply = None;

        Ok(self.make_ready(ri, TaskState::WaitingRqb, cpu))
    }

    /// If task `idx` is in `from`, make it Ready; return an idle CPU's
    /// APIC ID to kick so the wakeup is serviced promptly.
    fn make_ready(&mut self, idx: usize, from: TaskState, cpu: usize) -> Option<u32> {
        if self.tasks[idx].state == from {
            self.tasks[idx].state = TaskState::Ready;
            self.find_idle_cpu(cpu)
        } else {
            None
        }
    }

    /// Pop a pending request for the current task, or mark it WaitingRqb
    fn take_request(&mut self, cpu: usize, block: bool) -> Option<Rqb> {
        let me = self.current_idx(cpu);
        let t = &mut self.tasks[me];
        match t.inbox.pop_front() {
            Some(m) => Some(m),
            None => {
                if block {
                    t.state = TaskState::WaitingRqb;
                }
                None
            }
        }
    }

    /// Deliver a reply to `sender`, which must be blocked waiting on us
    fn post_reply(&mut self, cpu: usize, sender: u32, msg: &Rqb) -> Result<Option<u32>, RqbStatus> {
        let me = self.current_idx(cpu);
        let my_id = self.tasks[me].id;
        let si = self.find_live(sender).ok_or(RqbStatus::NotFound)?;
        let s = &mut self.tasks[si];
        if s.state != TaskState::WaitingReply || s.wait_for != my_id || s.reply.is_some() {
            return Err(RqbStatus::InvalidParam); // not waiting on a reply from us
        }
        let mut m = msg.clone();
        m.sender_id = my_id;
        m.receiver_id = sender;
        s.reply = Some(m);
        Ok(self.make_ready(si, TaskState::WaitingReply, cpu))
    }

    /// Mark the current task on this CPU as exited, and fail every request
    /// that can no longer be answered: senders blocked on this task (queued
    /// or already received) get an Aborted reply instead of hanging.
    fn mark_exited(&mut self, cpu: usize) -> Option<u32> {
        let me = self.current_idx(cpu);
        let my_id = self.tasks[me].id;
        self.tasks[me].state = TaskState::Exited;
        self.tasks[me].inbox.clear();

        let mut kick = None;
        for i in 0..self.tasks.len() {
            let t = &mut self.tasks[i];
            if t.state == TaskState::WaitingReply && t.wait_for == my_id && t.reply.is_none() {
                let mut r = Rqb::new();
                r.set_status(RqbStatus::Aborted);
                r.sender_id = my_id;
                r.receiver_id = t.id;
                t.reply = Some(r);
                kick = kick.or(self.make_ready(i, TaskState::WaitingReply, cpu));
            }
        }
        kick
    }

    /// Put the current task to sleep until jiffy `until`
    fn sleep_until(&mut self, cpu: usize, until: u64) {
        let me = self.current_idx(cpu);
        let t = &mut self.tasks[me];
        t.wake_tick = until;
        t.state = TaskState::Sleeping;
    }
}

// ========================================================================
// Global Scheduler Instance
// ========================================================================

use spin::Mutex;

static SCHEDULER: Mutex<Scheduler> = Mutex::new(Scheduler::new());

/// Run a closure with the scheduler locked and interrupts disabled on the
/// calling CPU (the only safe way to take the lock outside an interrupt).
fn with_scheduler<R>(f: impl FnOnce(&mut Scheduler) -> R) -> R {
    crate::arch::without_interrupts(|| {
        let mut sched = SCHEDULER.lock();
        f(&mut sched)
    })
}

/// Initialize the scheduler on the boot CPU: register CPU 0 (adopting the
/// boot context as its idle task) and start its tick source.
pub fn init() {
    let hw_id = crate::arch::hw_cpu_id();
    crate::arch::set_cpu_index(0);
    let already = with_scheduler(|sched| {
        if sched.initialized.swap(true, Ordering::SeqCst) {
            return true;
        }
        sched.register_cpu(0, hw_id);
        false
    });
    if already {
        return;
    }

    crate::arch::start_tick(SCHEDULER_FREQUENCY_HZ, true);

    crate::serial::write_str("[SCHED] Scheduler ready: ");
    crate::serial::write_dec(MAX_TASKS as u64);
    crate::serial::write_str(" max tasks at ");
    crate::serial::write_dec(SCHEDULER_FREQUENCY_HZ as u64);
    crate::serial::write_str(" Hz per CPU\n");
}

/// Register a secondary CPU with the scheduler and start its local timer
/// tick. Called on that CPU, with interrupts disabled; its park loop
/// becomes its idle task.
pub fn register_ap(cpu: usize) {
    let hw_id = crate::arch::hw_cpu_id();
    crate::arch::set_cpu_index(cpu);
    with_scheduler(|sched| sched.register_cpu(cpu, hw_id));
    crate::arch::start_tick(SCHEDULER_FREQUENCY_HZ, false);
}

/// Context-switch entry from an interrupt (timer tick, reschedule IPI).
/// The architecture's handler has saved the full context at `current_sp`
/// and acknowledged the interrupt.
///
/// Returns with the scheduler lock still held — the asm switches to the
/// new stack and then calls `scheduler_unlock`. This prevents another CPU
/// from resuming the outgoing task while its old stack is still in use.
///
/// # Safety
/// Only from interrupt context, with `current_sp` pointing at the saved
/// context on the current task's stack.
pub unsafe fn switch_from_interrupt(current_sp: u64, timer: bool) -> u64 {
    let cpu = current_cpu();
    let mut sched = SCHEDULER.lock();
    let new_sp = sched.on_tick(cpu, current_sp, timer);
    // Keep holding the lock across the stack switch (see scheduler_unlock)
    core::mem::forget(sched);
    SWITCH_LOCK_HELD[cpu].store(true, Ordering::Relaxed);
    new_sp
}

/// Voluntary-yield counterpart of `switch_from_interrupt`: same
/// lock-across-switch protocol, no tick accounting.
///
/// # Safety
/// Only called from the architecture's yield stub.
#[no_mangle]
pub unsafe extern "C" fn yield_and_switch(current_sp: u64) -> u64 {
    switch_from_interrupt(current_sp, false)
}

/// Give up the CPU. Returns when the scheduler next picks this task —
/// immediately if it is still Ready and nothing else is runnable.
///
/// Must be called with interrupts enabled and no spinlocks held.
pub fn yield_now() {
    crate::arch::yield_now();
}

/// Second half of the context switch: releases the scheduler lock taken by
/// `switch_from_interrupt`, once the asm has moved onto the new task's
/// stack. A no-op if this CPU's interrupt didn't switch (e.g. a device
/// IRQ on arm64), so every interrupt exit path may call it.
#[no_mangle]
pub unsafe extern "C" fn scheduler_unlock() {
    let cpu = current_cpu();
    if !SWITCH_LOCK_HELD[cpu].swap(false, Ordering::Relaxed) {
        return;
    }
    SCHEDULER.force_unlock();
}

/// Create a new task (public API). Wakes an idle CPU with a reschedule IPI
/// so the task starts running immediately.
pub fn create_task(entry: extern "C" fn() -> !, priority: TaskPriority, name: &'static str) -> u32 {
    // Allocate the stack outside the scheduler lock
    let stack = alloc_stack();
    let task = TaskControlBlock::new(entry, stack, priority, name);
    let stack_bottom = task.kernel_stack_bottom;

    let (tid, ipi_target) = with_scheduler(|sched| {
        let tid = sched.add_task(task);
        (tid, sched.find_idle_cpu(current_cpu()))
    });

    // One write, so the line can't interleave with tasks on other CPUs
    {
        use core::fmt::Write;
        let mut line: heapless::String<96> = heapless::String::new();
        let _ = write!(
            line,
            "[SCHED] Created task \"{}\" TID={} prio={} stack=0x{:x}\n",
            name, tid, priority, stack_bottom
        );
        crate::serial::write_str(&line);
    }

    // Kick an idle CPU so it picks the task up right away
    kick(ipi_target);

    tid
}

/// Create a user-mode task that starts at `entry` on `user_rsp`, with `arg`
/// in RDI. The caller has already mapped the code and stack as user pages.
pub fn create_user_task(entry: u64, user_rsp: u64, arg: u64, name: &'static str) -> u32 {
    let stack = alloc_stack();
    let task = TaskControlBlock::new_user(entry, user_rsp, arg, stack, PRIORITY_DEFAULT, name);
    let (tid, ipi_target) = with_scheduler(|sched| {
        let tid = sched.add_task(task);
        (tid, sched.find_idle_cpu(current_cpu()))
    });

    {
        use core::fmt::Write;
        let mut line: heapless::String<96> = heapless::String::new();
        let _ = write!(
            line,
            "[SCHED] Created user task \"{}\" TID={} entry=0x{:x} (user mode)\n",
            name, tid, entry
        );
        crate::serial::write_str(&line);
    }

    kick(ipi_target);
    tid
}

/// State of a task by ID (None if no such task)
pub fn task_state(tid: u32) -> Option<TaskState> {
    with_scheduler(|s| s.tasks.iter().find(|t| t.id == tid).map(|t| t.state))
}

/// Name of the task running on this CPU
pub fn current_task_name() -> &'static str {
    with_scheduler(|s| {
        let cpu = current_cpu();
        s.cpus[cpu].current.map(|i| s.tasks[i].name).unwrap_or("?")
    })
}

/// Allocate a task stack from the kernel heap
fn alloc_stack() -> Box<[u8]> {
    let layout = alloc::alloc::Layout::from_size_align(TASK_STACK_SIZE, 16)
        .expect("Invalid stack layout");
    let ptr = unsafe { alloc::alloc::alloc_zeroed(layout) };
    assert!(!ptr.is_null(), "OOM allocating task stack");
    unsafe {
        let slice = core::slice::from_raw_parts_mut(ptr, TASK_STACK_SIZE);
        Box::from_raw(slice)
    }
}

/// Get the current task ID on the calling CPU
pub fn current_task_id() -> u32 {
    with_scheduler(|sched| sched.current_id(current_cpu()))
}

/// Send a reschedule IPI to wake an idle CPU (outside the scheduler lock)
fn kick(target: Option<u32>) {
    if let Some(hw_id) = target {
        if crate::arch::ipi_available() {
            crate::arch::send_resched_ipi(hw_id);
        }
    }
}

/// Send a request to `receiver` and block until it replies (MMURTL's
/// synchronous Request/Respond). On return `rqb` holds the reply and the
/// returned status is the reply's status — or an IPC error (NotFound,
/// Busy, InvalidParam) if the request was never delivered, or Aborted if
/// the receiver exited before answering.
pub fn send_rqb(receiver: u32, rqb: &mut Rqb) -> RqbStatus {
    match with_scheduler(|s| s.post_request(current_cpu(), receiver, rqb)) {
        Ok(target) => kick(target),
        Err(status) => {
            rqb.set_status(status);
            return status;
        }
    }

    loop {
        yield_now();
        // We only run again once Ready — i.e. a reply (or an Aborted
        // notice) is in our slot. Re-check under the lock regardless.
        let reply = with_scheduler(|s| {
            let me = s.current_idx(current_cpu());
            let t = &mut s.tasks[me];
            let r = t.reply.take();
            if r.is_none() {
                t.state = TaskState::WaitingReply; // spurious wake: re-block
            }
            r
        });
        if let Some(r) = reply {
            *rqb = r;
            return RqbStatus::from(rqb.status);
        }
    }
}

/// Block until a request arrives, and return it. `rqb.sender_id` names
/// the task to `reply_rqb` to.
pub fn receive_rqb() -> Rqb {
    loop {
        if let Some(m) = with_scheduler(|s| s.take_request(current_cpu(), true)) {
            return m;
        }
        yield_now();
    }
}

/// Non-blocking receive: a pending request, if any
pub fn try_receive_rqb() -> Option<Rqb> {
    with_scheduler(|s| s.take_request(current_cpu(), false))
}

/// Reply to a sender blocked in `send_rqb` on us. Fails with NotFound if
/// the sender is gone, InvalidParam if it isn't awaiting our reply.
pub fn reply_rqb(sender: u32, rqb: &Rqb) -> RqbStatus {
    match with_scheduler(|s| s.post_reply(current_cpu(), sender, rqb)) {
        Ok(target) => {
            kick(target);
            RqbStatus::Success
        }
        Err(e) => e,
    }
}

/// Mark the current task as exited (see `task::exit_current`)
pub fn mark_current_exited() {
    let target = with_scheduler(|s| s.mark_exited(current_cpu()));
    kick(target);
}

/// Block the current task for at least `ms` milliseconds
pub fn sleep_ms(ms: u64) {
    let ticks = ((ms * SCHEDULER_FREQUENCY_HZ as u64) + 999) / 1000;
    let until = jiffies() + ticks.max(1);
    while jiffies() < until {
        with_scheduler(|s| s.sleep_until(current_cpu(), until));
        yield_now();
    }
}
