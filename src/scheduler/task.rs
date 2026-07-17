//! Task Control Block — Per-task state and management.
//!
//! MMURTL's process model: all tasks run in the same address space (kernel mode),
//! with each task having its own kernel stack. The scheduler switches between
//! tasks by swapping RSP and the saved register context.

use alloc::boxed::Box;
use core::fmt;

// ========================================================================
// Task States
// ========================================================================

/// The state of a task in the scheduler
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TaskState {
    /// Task is ready to run
    Ready = 0,
    /// Task is currently running
    Running = 1,
    /// Task is waiting for an RQB reply
    WaitingRqb = 2,
    /// Task is waiting for a specific amount of time
    Sleeping = 3,
    /// Task has exited
    Exited = 4,
    /// Task is blocked on a resource
    Blocked = 5,
}

impl TaskState {
    pub const fn is_active(self) -> bool {
        matches!(self, Self::Ready | Self::Running)
    }
}

// ========================================================================
// Task Priority
// ========================================================================

/// Task priority levels (0 = highest, 31 = lowest, default = 16)
pub type TaskPriority = u8;

pub const PRIORITY_HIGHEST: TaskPriority = 0;
pub const PRIORITY_DEFAULT: TaskPriority = 16;
pub const PRIORITY_LOWEST: TaskPriority = 31;
pub const PRIORITY_IDLE: TaskPriority = 31;

// ========================================================================
// Task ID Generation
// =======================================================================+

static NEXT_TASK_ID: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(1);

fn next_task_id() -> u32 {
    NEXT_TASK_ID.fetch_add(1, core::sync::atomic::Ordering::Relaxed)
}

// ========================================================================
// Task Control Block (TCB)
// ========================================================================

/// Saved register context for a task (in order pushed by context switch)
#[repr(C)]
pub struct TaskContext {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rbp: u64,
    pub rbx: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rax: u64,
    // Below this are the interrupt frame (pushed by CPU on interrupt)
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

/// Task Control Block — describes a single execution context
#[repr(C)]
pub struct TaskControlBlock {
    /// Task ID (unique)
    pub id: u32,
    /// Current state
    pub state: TaskState,
    /// Priority (0=highest, 31=lowest)
    pub priority: TaskPriority,
    /// Points to TaskContext on the task's kernel stack (raw address)
    pub context_ptr: u64,
    /// Top of the task's kernel stack (highest address)
    pub kernel_stack_top: u64,
    /// Bottom of the task's kernel stack (lowest address)
    pub kernel_stack_bottom: u64,
    /// Name for debugging
    pub name: &'static str,
    /// Total ticks this task has run
    pub total_ticks: u64,
    /// If set, this task may only run on the given CPU (used for per-CPU
    /// idle tasks). None = may run anywhere.
    pub pinned_cpu: Option<u8>,
}

impl TaskControlBlock {
    /// Create a new task with the given entry point and stack.
    ///
    /// The stack must be a valid executable memory region. The entry point
    /// is wrapped in a function that calls `task_entry_point` so that when
    /// the task returns, it calls `exit_current_task()`.
    ///
    /// Takes ownership of a `Box<[u8]>` for the stack so the stack is
    /// automatically freed when the task is destroyed.
    pub fn new(
        entry: extern "C" fn() -> !,
        stack: Box<[u8]>,
        priority: TaskPriority,
        name: &'static str,
    ) -> Box<Self> {
        // Leak the box to get a static mut reference we can use
        let stack = Box::leak(stack);
        let stack_top = stack.as_ptr() as u64 + stack.len() as u64;
        let stack_bottom = stack.as_ptr() as u64;

        // Build an initial context on the task's stack.
        // The context should look like it was saved by the timer interrupt handler,
        // so that the first context switch into this task just returns through IRETQ.
        //
        // Stack layout (from low to high addr):
        //   [TaskContext] ← RSP points here (context_ptr)
        //   [unused space toward stack_top]
        //
        // The saved RIP should point to `entry`. The saved RFLAGS should
        // have IF set so interrupts are enabled when the task runs.
        //
        // We place the TaskContext at the bottom of the stack area.
        let ctx_addr = stack_bottom;
        unsafe {
            let ctx_ptr = ctx_addr as *mut TaskContext;
            ctx_ptr.write(TaskContext {
                rax: 0,
                rbx: 0,
                rcx: 0,
                rdx: 0,
                rsi: 0,
                rdi: 0,
                rbp: 0,
                r8: 0,
                r9: 0,
                r10: 0,
                r11: 0,
                r12: 0,
                r13: 0,
                r14: 0,
                r15: 0,
                rip: entry as u64,
                cs: 0x08, // GDT kernel code segment selector
                rflags: 0x202, // IF (interrupts enabled) + reserved bit 1
                rsp: stack_top, // Unused — IRETQ will set RSP from here
                ss: 0x10, // GDT kernel data segment selector
            });
        }

        Box::new(Self {
            id: next_task_id(),
            state: TaskState::Ready,
            priority,
            context_ptr: ctx_addr,
            kernel_stack_top: stack_top,
            kernel_stack_bottom: stack_bottom,
            name,
            total_ticks: 0,
            pinned_cpu: None,
        })
    }

    /// Adopt the currently-executing context as a task.
    ///
    /// Used for per-CPU idle tasks: the CPU's boot/park loop *becomes* the
    /// idle task. No initial context is crafted — `context_ptr` is filled
    /// in the first time the timer interrupt saves this context.
    pub fn adopt_current(name: &'static str, priority: TaskPriority, cpu: u8) -> Box<Self> {
        Box::new(Self {
            id: next_task_id(),
            state: TaskState::Running,
            priority,
            context_ptr: 0,
            kernel_stack_top: 0,
            kernel_stack_bottom: 0,
            name,
            total_ticks: 0,
            pinned_cpu: Some(cpu),
        })
    }
}

impl fmt::Debug for TaskControlBlock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TCB#{} \"{}\" {:?} prio={} ticks={}",
            self.id, self.name, self.state, self.priority, self.total_ticks)
    }
}

// ========================================================================
// Task Entry/Exit Helpers
// =======================================================================+

/// The default initial entry point for tasks.
/// This calls the user's entry function and, if it returns, marks the task as exited.
pub extern "C" fn task_wrapper(entry: extern "C" fn() -> !) -> ! {
    entry()
}

/// Current task exit — called when a task function returns or voluntarily exits
pub fn exit_current() {
    crate::serial::write_str("[SCHED] Task exited\n");
    crate::scheduler::mark_current_exited();
    loop {
        x86_64::instructions::hlt();
    }
}
