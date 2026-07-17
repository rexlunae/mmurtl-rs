//! Scheduler — Preemptive round-robin task scheduler with RQB IPC.
//!
//! The scheduler uses the PIT (Programmable Interval Timer) to trigger
//! context switches at ~100 Hz. Each task gets a fixed time slice.
//! Tasks communicate via RQB (Request Block) message passing.
//!
//! Context switching is done by saving/restoring all registers on the
//! task's kernel stack and swapping RSP.

mod task;
mod rqb;

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

pub use task::*;
pub use rqb::*;

// ========================================================================
// Scheduler Constants
// ========================================================================

/// Default stack size for each task (32 KiB)
pub const TASK_STACK_SIZE: usize = 32 * 1024;

/// Maximum number of tasks
pub const MAX_TASKS: usize = 64;

/// PIT timer frequency (Hz)
pub const SCHEDULER_FREQUENCY_HZ: u32 = 100;

/// Task quantum in ticks
pub const TASK_QUANTUM_TICKS: u32 = 3; // ~30ms at 100Hz

// ========================================================================
// Scheduler State
// =======================================================================+

/// Per-task message queue entry
struct MessageEntry {
    sender_id: u32,
    rqb: Rqb,
}

/// The global scheduler
pub struct Scheduler {
    /// All registered tasks
    tasks: Vec<Box<TaskControlBlock>>,
    /// Index of the currently running task
    current_index: usize,
    /// Number of ticks since scheduler start
    tick_count: u64,
    /// Whether the scheduler is initialized
    initialized: AtomicBool,
}

impl Scheduler {
    const fn new() -> Self {
        Self {
            tasks: Vec::new(),
            current_index: 0,
            tick_count: 0,
            initialized: AtomicBool::new(false),
        }
    }

    /// Initialize the scheduler: set up the idle task and enable the timer
    fn init(&mut self) {
        if self.initialized.swap(true, Ordering::SeqCst) {
            return;
        }

        crate::serial::write_str("[SCHED] Initializing scheduler...\n");

        // Create the idle task (lowest priority)
        let idle_stack = Self::alloc_stack("idle");
        let idle_task = TaskControlBlock::new(
            idle_task_entry,
            idle_stack,
            PRIORITY_IDLE,
            "idle",
        );
        crate::serial::write_str("[SCHED] Created idle task (TID=");
        crate::serial::write_dec(idle_task.id as u64);
        crate::serial::write_str(")\n");
        self.tasks.push(idle_task);

        // Start the tick source: LAPIC timer in APIC mode, PIT otherwise
        if crate::apic::enabled() {
            crate::apic::start_timer(SCHEDULER_FREQUENCY_HZ);
        } else {
            self.init_pit();
        }

        crate::serial::write_str("[SCHED] Scheduler ready: ");
        crate::serial::write_dec(MAX_TASKS as u64);
        crate::serial::write_str(" max tasks at ");
        crate::serial::write_dec(SCHEDULER_FREQUENCY_HZ as u64);
        crate::serial::write_str(" Hz\n");
    }

    /// Create a new task and add it to the scheduler
    fn create_task(
        &mut self,
        entry: extern "C" fn() -> !,
        priority: TaskPriority,
        name: &'static str,
    ) -> u32 {
        let stack = Self::alloc_stack(name);
        let task = TaskControlBlock::new(entry, stack, priority, name);
        let tid = task.id;

        crate::serial::write_str("[SCHED] Created task \"");
        crate::serial::write_str(name);
        crate::serial::write_str("\" TID=");
        crate::serial::write_dec(tid as u64);
        crate::serial::write_str(" prio=");
        crate::serial::write_dec(priority as u64);
        crate::serial::write_str(" stack=0x");
        crate::serial::write_hex(task.kernel_stack_bottom);
        crate::serial::write_str("\n");

        self.tasks.push(task);
        tid
    }

    /// Allocate a stack for a new task (from the kernel heap)
    fn alloc_stack(name: &str) -> Box<[u8]> {
        crate::serial::write_str("[SCHED]   alloc_stack(");
        crate::serial::write_str(name);
        crate::serial::write_str(")... ");

        let layout = alloc::alloc::Layout::from_size_align(TASK_STACK_SIZE, 16)
            .expect("Invalid stack layout");
        crate::serial::write_str("layout_ok ");

        let ptr = unsafe { alloc::alloc::alloc_zeroed(layout) };
        crate::serial::write_str("zeroed ");

        // Build a Box<[u8]> from the raw allocation
        let slice = unsafe {
            core::slice::from_raw_parts_mut(ptr, TASK_STACK_SIZE)
        };
        crate::serial::write_str("slice ");

        let boxed: Box<[u8]> = unsafe {
            Box::from_raw(slice)
        };
        crate::serial::write_str("boxed ");

        // Log allocation
        crate::serial::write_str("Stack for \"");
        crate::serial::write_str(name);
        crate::serial::write_str("\": ");
        crate::serial::write_dec(TASK_STACK_SIZE as u64);
        crate::serial::write_str(" bytes at 0x");
        crate::serial::write_hex(boxed.as_ptr() as u64);
        crate::serial::write_str("\n");

        boxed
    }

    /// Initialize the PIT (8253) to fire at SCHEDULER_FREQUENCY_HZ
    fn init_pit(&self) {
        use x86_64::instructions::port::Port;

        // PIT frequency: 1.193182 MHz base clock
        // Divisor = 1,193,182 / desired Hz
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

            // Program divisor (low byte then high byte)
            let mut data_port: Port<u8> = Port::new(0x40);
            data_port.write((divisor & 0xFF) as u8);
            data_port.write(((divisor >> 8) & 0xFF) as u8);
        }
    }

    /// Called on each timer tick — perform scheduling decision
    ///
    /// # Safety
    /// Only called from the timer interrupt handler (which holds the interrupt context).
    /// Takes the current RSP (pointing to saved TaskContext) and returns the new RSP as u64.
    pub unsafe fn on_tick(&mut self, current_rsp: u64) -> u64 {
        if self.tasks.is_empty() {
            return current_rsp;
        }

        self.tick_count += 1;

        // Update current task's context pointer
        let current_tcb = &mut self.tasks[self.current_index];
        current_tcb.context_ptr = current_rsp;
        current_tcb.state = TaskState::Ready;
        current_tcb.total_ticks += 1;

        // Pick the next task to run
        let next_idx = self.pick_next();
        self.current_index = next_idx;
        let next_tcb = &mut self.tasks[next_idx];
        next_tcb.state = TaskState::Running;

        // Return the next task's context pointer (new RSP value)
        next_tcb.context_ptr
    }

    /// Pick the next task to run (priority-based round-robin)
    fn pick_next(&self) -> usize {
        let n = self.tasks.len();
        if n == 0 {
            return 0;
        }

        // Round-robin: advance to the next active task.
        // Skip exited tasks, waiting tasks, and idle-priority tasks
        // if any non-idle task is ready first.
        let mut idle_fallback = None;
        for offset in 1..=n {
            let idx = (self.current_index + offset) % n;
            let t = &self.tasks[idx];
            match t.state {
                TaskState::Ready | TaskState::Running => {
                    if t.priority == PRIORITY_IDLE {
                        // Remember idle as a fallback
                        if idle_fallback.is_none() {
                            idle_fallback = Some(idx);
                        }
                        continue;
                    }
                    return idx;
                }
                _ => continue,
            }
        }

        // No non-idle ready task — use idle fallback or current
        idle_fallback.unwrap_or(self.current_index)
    }

    /// Get the current task ID
    fn current_id(&self) -> u32 {
        if self.tasks.is_empty() {
            0
        } else {
            self.tasks[self.current_index].id
        }
    }

    /// Send a message to a task (blocks caller until reply)
    fn send_msg(&mut self, receiver_id: u32, rqb: &mut Rqb) {
        // Find the receiver by ID
        if let Some(receiver_idx) = self.tasks.iter().position(|t| t.id == receiver_id) {
            // In a full implementation, we'd queue the RQB and block the sender
            // For now, we directly copy the RQB and let the caller handle it
            // (Simple rendezvous: if receiver is waiting, deliver immediately)
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
    fn recv_msg(&mut self, rqb: &mut Rqb) {
        // In a full implementation, check message queue and block if empty
        // For now, just mark as waiting and the scheduler will skip us
        let current = &mut self.tasks[self.current_index];
        current.state = TaskState::WaitingRqb;
        // The actual message delivery happens in send_msg
        // (This is a simplified rendezvous model)
    }

    /// Reply to a sender
    fn reply_msg(&mut self, sender_id: u32, rqb: &Rqb) {
        // Find the sender and copy the reply
        if let Some(sender_idx) = self.tasks.iter().position(|t| t.id == sender_id) {
            self.tasks[sender_idx].state = TaskState::Ready;
        }
    }

    /// Mark the current task as exited
    fn mark_exited(&mut self) {
        if !self.tasks.is_empty() {
            self.tasks[self.current_index].state = TaskState::Exited;
        }
    }
}

// ========================================================================
// Global Scheduler Instance
// =======================================================================+

use spin::Mutex;

static SCHEDULER: Mutex<Scheduler> = Mutex::new(Scheduler::new());

/// Initialize the scheduler
pub fn init() {
    let mut sched = SCHEDULER.lock();
    sched.init();

    if !crate::apic::enabled() {
        // Legacy mode: unmask the timer IRQ in the PIC
        unsafe {
            let mut pic1_data: x86_64::instructions::port::Port<u8> =
                x86_64::instructions::port::Port::new(0x21);
            // Clear bit 0 (IRQ0 timer) to unmask it
            let mask = pic1_data.read();
            pic1_data.write(mask & !0x01);
        }
        crate::serial::write_str("[PIC] Timer IRQ0 unmasked\n");
    }
}

/// Called from the timer interrupt's naked handler to perform scheduling.
///
/// # Safety
/// This function must only be called from the timer interrupt handler
/// with RSP pointing to a valid TaskContext on the current task's stack.
#[no_mangle]
pub unsafe extern "C" fn schedule_and_switch(current_rsp: u64) -> u64 {
    // Acknowledge the timer interrupt (LAPIC EOI in APIC mode, PIC otherwise)
    crate::interrupts::irq_eoi(0);

    let mut sched = SCHEDULER.lock();
    sched.on_tick(current_rsp)
}

/// Create a new task (public API)
pub fn create_task(entry: extern "C" fn() -> !, priority: TaskPriority, name: &'static str) -> u32 {
    let was_enabled = x86_64::instructions::interrupts::are_enabled();
    x86_64::instructions::interrupts::disable();
    let mut sched = SCHEDULER.lock();
    let tid = sched.create_task(entry, priority, name);
    drop(sched);
    if was_enabled {
        x86_64::instructions::interrupts::enable();
    }
    tid
}

/// Get the current task ID
pub fn current_task_id() -> u32 {
    let was_enabled = x86_64::instructions::interrupts::are_enabled();
    x86_64::instructions::interrupts::disable();
    let sched = SCHEDULER.lock();
    let id = sched.current_id();
    drop(sched);
    if was_enabled {
        x86_64::instructions::interrupts::enable();
    }
    id
}

/// Send a message (blocking)
pub fn send_message(receiver_id: u32, rqb: &mut Rqb) {
    let was_enabled = x86_64::instructions::interrupts::are_enabled();
    x86_64::instructions::interrupts::disable();
    let mut sched = SCHEDULER.lock();
    sched.send_msg(receiver_id, rqb);
    drop(sched);
    if was_enabled {
        x86_64::instructions::interrupts::enable();
    }
}

/// Receive a message (blocking)
pub fn receive_message(rqb: &mut Rqb) {
    let was_enabled = x86_64::instructions::interrupts::are_enabled();
    x86_64::instructions::interrupts::disable();
    let mut sched = SCHEDULER.lock();
    sched.recv_msg(rqb);
    drop(sched);
    if was_enabled {
        x86_64::instructions::interrupts::enable();
    }
}

/// Reply to a message
pub fn reply_message(sender_id: u32, rqb: &Rqb) {
    let was_enabled = x86_64::instructions::interrupts::are_enabled();
    x86_64::instructions::interrupts::disable();
    let mut sched = SCHEDULER.lock();
    sched.reply_msg(sender_id, rqb);
    drop(sched);
    if was_enabled {
        x86_64::instructions::interrupts::enable();
    }
}

/// Mark the current task as exited
pub fn mark_current_exited() {
    let was_enabled = x86_64::instructions::interrupts::are_enabled();
    x86_64::instructions::interrupts::disable();
    let mut sched = SCHEDULER.lock();
    sched.mark_exited();
    drop(sched);
    if was_enabled {
        x86_64::instructions::interrupts::enable();
    }
}

/// Get the current RQB wait status (placeholder)
pub fn current_rqb_status() -> RqbStatus {
    RqbStatus::Success
}

// ========================================================================
// Idle Task
// =======================================================================+

/// The idle task — runs when nothing else can
extern "C" fn idle_task_entry() -> ! {
    loop {
        x86_64::instructions::hlt();
    }
}
