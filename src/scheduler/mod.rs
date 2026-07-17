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
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

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

// ========================================================================
// Per-CPU state
// ========================================================================

#[derive(Clone, Copy)]
struct PerCpu {
    /// Whether this CPU has registered with the scheduler
    registered: bool,
    /// This CPU's Local APIC ID (for sending it IPIs)
    apic_id: u32,
    /// Index of the task currently running on this CPU
    current: Option<usize>,
    /// Index of this CPU's pinned idle task
    idle_idx: usize,
}

impl PerCpu {
    const EMPTY: Self = Self {
        registered: false,
        apic_id: 0,
        current: None,
        idle_idx: 0,
    };
}

/// APIC ID → CPU index, written at CPU registration, read lock-free on
/// every timer tick. Index by APIC ID (xAPIC IDs are < 256).
static APIC_TO_CPU: [AtomicU32; 256] = {
    const ZERO: AtomicU32 = AtomicU32::new(0);
    [ZERO; 256]
};

/// The CPU index of the calling processor
pub fn current_cpu() -> usize {
    if !crate::apic::enabled() {
        return 0;
    }
    let apic_id = crate::apic::local_apic_id() as usize;
    APIC_TO_CPU[apic_id & 0xFF].load(Ordering::Relaxed) as usize
}

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
    fn register_cpu(&mut self, cpu: usize, apic_id: u32) -> u32 {
        let idle = TaskControlBlock::adopt_current("idle", PRIORITY_IDLE, cpu as u8);
        let tid = idle.id;
        self.tasks.push(idle);
        let idx = self.tasks.len() - 1;

        self.cpus[cpu] = PerCpu {
            registered: true,
            apic_id,
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

    /// Called on each timer tick / reschedule IPI on any CPU.
    ///
    /// # Safety
    /// Only called from interrupt context with the scheduler lock held.
    /// Takes the current RSP (pointing to saved TaskContext) and returns
    /// the next task's context pointer as the new RSP.
    pub unsafe fn on_tick(&mut self, cpu: usize, current_rsp: u64) -> u64 {
        if !self.cpus[cpu].registered || self.tasks.is_empty() {
            return current_rsp;
        }

        self.tick_count += 1;

        // Save the interrupted context into the current task
        if let Some(cur) = self.cpus[cpu].current {
            let t = &mut self.tasks[cur];
            t.context_ptr = current_rsp;
            t.total_ticks += 1;
            if t.state == TaskState::Running {
                t.state = TaskState::Ready;
            }
        }

        // Pick the next task for this CPU
        let next = self.pick_next(cpu);
        self.tasks[next].state = TaskState::Running;
        self.cpus[cpu].current = Some(next);
        self.tasks[next].context_ptr
    }

    /// Pick the next task for `cpu`: round-robin over Ready, unpinned (or
    /// pinned-here), non-idle tasks; fall back to this CPU's idle task.
    fn pick_next(&mut self, cpu: usize) -> usize {
        let n = self.tasks.len();
        for offset in 1..=n {
            let idx = (self.rr_cursor + offset) % n;
            let t = &self.tasks[idx];
            if t.state != TaskState::Ready {
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
                return Some(c.apic_id);
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

    /// Send a message to a task (blocks caller until reply)
    fn send_msg(&mut self, receiver_id: u32, rqb: &mut Rqb) {
        if let Some(receiver_idx) = self.tasks.iter().position(|t| t.id == receiver_id) {
            if self.tasks[receiver_idx].state == TaskState::WaitingRqb {
                // Receiver is waiting — deliver the message
                self.tasks[receiver_idx].state = TaskState::Ready;
                rqb.status = RqbStatus::Success as u16;
            } else {
                // Receiver not waiting — queue the message
                // (Simplified: just mark status and continue)
                rqb.status = RqbStatus::Success as u16;
            }
        } else {
            rqb.status = RqbStatus::NotFound as u16;
        }
    }

    /// Receive a message (blocks until one arrives)
    fn recv_msg(&mut self, cpu: usize, _rqb: &mut Rqb) {
        // In a full implementation, check message queue and block if empty
        // For now, just mark as waiting and the scheduler will skip us
        if let Some(idx) = self.cpus[cpu].current {
            self.tasks[idx].state = TaskState::WaitingRqb;
        }
    }

    /// Reply to a sender
    fn reply_msg(&mut self, sender_id: u32, _rqb: &Rqb) {
        if let Some(sender_idx) = self.tasks.iter().position(|t| t.id == sender_id) {
            self.tasks[sender_idx].state = TaskState::Ready;
        }
    }

    /// Mark the current task on this CPU as exited
    fn mark_exited(&mut self, cpu: usize) {
        if let Some(idx) = self.cpus[cpu].current {
            self.tasks[idx].state = TaskState::Exited;
        }
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
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut sched = SCHEDULER.lock();
        f(&mut sched)
    })
}

/// Initialize the scheduler on the BSP: register CPU 0 (adopting the boot
/// context as its idle task) and start the tick source.
pub fn init() {
    let apic_mode = crate::apic::enabled();
    let bsp_apic_id = if apic_mode { crate::apic::local_apic_id() } else { 0 };

    let already = with_scheduler(|sched| {
        if sched.initialized.swap(true, Ordering::SeqCst) {
            return true;
        }
        sched.register_cpu(0, bsp_apic_id);
        false
    });
    if already {
        return;
    }
    APIC_TO_CPU[(bsp_apic_id & 0xFF) as usize].store(0, Ordering::Relaxed);

    // Start the tick source: LAPIC timer in APIC mode, PIT otherwise
    if apic_mode {
        crate::apic::start_timer(SCHEDULER_FREQUENCY_HZ);
    } else {
        init_pit();
        unsafe {
            // Legacy mode: unmask the timer IRQ in the PIC
            let mut pic1_data: x86_64::instructions::port::Port<u8> =
                x86_64::instructions::port::Port::new(0x21);
            let mask = pic1_data.read();
            pic1_data.write(mask & !0x01);
        }
        crate::serial::write_str("[PIC] Timer IRQ0 unmasked\n");
    }

    crate::serial::write_str("[SCHED] Scheduler ready: ");
    crate::serial::write_dec(MAX_TASKS as u64);
    crate::serial::write_str(" max tasks at ");
    crate::serial::write_dec(SCHEDULER_FREQUENCY_HZ as u64);
    crate::serial::write_str(" Hz per CPU\n");
}

/// Register an application processor with the scheduler and start its
/// local timer tick. Called from `ap_entry` with interrupts disabled;
/// the AP's park loop becomes its idle task.
pub fn register_ap(cpu: usize) {
    let apic_id = crate::apic::local_apic_id();
    with_scheduler(|sched| sched.register_cpu(cpu, apic_id));
    APIC_TO_CPU[(apic_id & 0xFF) as usize].store(cpu as u32, Ordering::Relaxed);

    // Per-CPU LAPIC timer, same frequency as the BSP
    crate::apic::start_timer(SCHEDULER_FREQUENCY_HZ);
}

/// Initialize the PIT (8253) to fire at SCHEDULER_FREQUENCY_HZ (fallback
/// tick source when there is no APIC)
fn init_pit() {
    use x86_64::instructions::port::Port;

    // PIT frequency: 1.193182 MHz base clock
    let divisor: u16 = (1193182u32 / SCHEDULER_FREQUENCY_HZ) as u16;

    crate::serial::write_str("[PIT] Frequency: ");
    crate::serial::write_dec(SCHEDULER_FREQUENCY_HZ as u64);
    crate::serial::write_str(" Hz (divisor=");
    crate::serial::write_dec(divisor as u64);
    crate::serial::write_str(")\n");

    unsafe {
        // Channel 0, lobyte/hibyte, mode 3 (square wave), binary mode
        let mut cmd_port: Port<u8> = Port::new(0x43);
        cmd_port.write(0x36u8);

        let mut data_port: Port<u8> = Port::new(0x40);
        data_port.write((divisor & 0xFF) as u8);
        data_port.write(((divisor >> 8) & 0xFF) as u8);
    }
}

/// Called from the timer/reschedule-IPI asm handler to perform scheduling.
///
/// Returns with the scheduler lock still held — the asm switches to the
/// new stack and then calls `scheduler_unlock`. This prevents another CPU
/// from resuming the outgoing task while its old stack is still in use.
///
/// # Safety
/// Must only be called from the interrupt handler with RSP pointing to a
/// valid TaskContext on the current task's stack.
#[no_mangle]
pub unsafe extern "C" fn schedule_and_switch(current_rsp: u64) -> u64 {
    // Acknowledge the interrupt (LAPIC EOI in APIC mode, PIC otherwise)
    crate::interrupts::irq_eoi(0);

    let cpu = current_cpu();
    let mut sched = SCHEDULER.lock();
    let new_rsp = sched.on_tick(cpu, current_rsp);
    // Keep holding the lock across the stack switch (see scheduler_unlock)
    core::mem::forget(sched);
    new_rsp
}

/// Second half of the context switch: releases the scheduler lock taken by
/// `schedule_and_switch`. Called from the timer asm after RSP now points
/// at the new task's stack.
#[no_mangle]
pub unsafe extern "C" fn scheduler_unlock() {
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

    crate::serial::write_str("[SCHED] Created task \"");
    crate::serial::write_str(name);
    crate::serial::write_str("\" TID=");
    crate::serial::write_dec(tid as u64);
    crate::serial::write_str(" prio=");
    crate::serial::write_dec(priority as u64);
    crate::serial::write_str(" stack=0x");
    crate::serial::write_hex(stack_bottom);
    crate::serial::write_str("\n");

    // Kick an idle CPU so it picks the task up right away
    if crate::apic::enabled() {
        if let Some(apic_id) = ipi_target {
            crate::apic::send_ipi(apic_id, crate::apic::RESCHED_VECTOR);
        }
    }

    tid
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

/// Send a message (blocking)
pub fn send_message(receiver_id: u32, rqb: &mut Rqb) {
    with_scheduler(|sched| sched.send_msg(receiver_id, rqb));
}

/// Receive a message (blocking)
pub fn receive_message(rqb: &mut Rqb) {
    with_scheduler(|sched| {
        let cpu = current_cpu();
        sched.recv_msg(cpu, rqb)
    });
}

/// Reply to a message
pub fn reply_message(sender_id: u32, rqb: &Rqb) {
    with_scheduler(|sched| sched.reply_msg(sender_id, rqb));
}

/// Mark the current task as exited
pub fn mark_current_exited() {
    with_scheduler(|sched| {
        let cpu = current_cpu();
        sched.mark_exited(cpu)
    });
}

/// Get the current RQB wait status (placeholder)
pub fn current_rqb_status() -> RqbStatus {
    RqbStatus::Success
}
