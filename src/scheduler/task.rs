//! Task Control Block — Per-task state and management.
//!
//! MMURTL's process model: all tasks run in the same address space (kernel mode),
//! with each task having its own kernel stack. The scheduler switches between
//! tasks by swapping RSP and the saved register context.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use core::fmt;

use super::rqb::Rqb;

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
    /// Task is blocked in `receive`, waiting for a message to arrive
    WaitingRqb = 2,
    /// Task is waiting for a specific amount of time
    Sleeping = 3,
    /// Task has exited
    Exited = 4,
    /// Task is blocked on a resource
    Blocked = 5,
    /// Task sent a request and is blocked until the receiver replies
    WaitingReply = 6,
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

/// Saved register context for a task — the architecture's exception
/// frame (general registers + return state), stored on its kernel stack
pub use crate::arch::TaskContext;

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
    /// CPU this task is executing on right now, or None when its context
    /// is saved. A task can be made Ready (by a reply/wakeup) while it is
    /// still executing on the way into a yield — the scheduler must not
    /// resume its stale saved context on another CPU until it has been
    /// switched out, so tasks with `on_cpu.is_some()` are never picked.
    pub on_cpu: Option<u8>,
    /// Pending requests sent to this task (delivered by `receive`)
    pub inbox: VecDeque<Rqb>,
    /// Reply slot, filled by the receiver's `reply` while we are in
    /// WaitingReply
    pub reply: Option<Rqb>,
    /// Task ID we are blocked on while in WaitingReply
    pub wait_for: u32,
    /// Jiffy at which a Sleeping task becomes Ready again
    pub wake_tick: u64,
    /// Ring-3 task: runs user code, enters the kernel via interrupts and
    /// `int 0x80` on its kernel stack (TSS.RSP0 = kernel_stack_top)
    pub user: bool,
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

        // Build an initial context on the task's stack, shaped exactly
        // like one the timer interrupt saves, so the first switch into the
        // task just "returns" into `entry` (IRETQ on amd64, ERET on arm64).
        //
        // Stack layout (from low to high addr):
        //   [free stack space — headroom below the context]
        //   [TaskContext] ← context_ptr
        //   = stack_top
        //
        // The context sits at the TOP of the stack, mirroring where a
        // preempted task's saved context lives. This headroom is
        // load-bearing: after the switch path points the stack at
        // context_ptr, it calls scheduler_unlock, which pushes a frame
        // BELOW context_ptr. With the context at stack_bottom those pushes
        // would land outside the allocation and corrupt the adjacent heap
        // object on the first switch into every new task.
        let ctx_addr = (stack_top - core::mem::size_of::<TaskContext>() as u64) & !0xF;
        debug_assert!(ctx_addr >= stack_bottom);
        unsafe {
            (ctx_addr as *mut TaskContext).write(crate::arch::kernel_context(entry as u64, stack_top));
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
            on_cpu: None,
            inbox: VecDeque::new(),
            reply: None,
            wait_for: 0,
            wake_tick: 0,
            user: false,
        })
    }

    /// Create a user-mode task (ring 3 / EL0). `stack` becomes its kernel
    /// stack (used for syscalls and interrupts taken from user mode); the
    /// initial context returns to `entry` in user mode on `user_rsp`, with
    /// `arg` in the first argument register.
    pub fn new_user(
        entry: u64,
        user_rsp: u64,
        arg: u64,
        stack: Box<[u8]>,
        priority: TaskPriority,
        name: &'static str,
    ) -> Box<Self> {
        // Reuse the kernel-task constructor for the stack bookkeeping, then
        // rewrite its initial frame for a privilege-level change
        let dummy: extern "C" fn() -> ! = user_entry_placeholder;
        let mut tcb = Self::new(dummy, stack, priority, name);
        unsafe {
            (tcb.context_ptr as *mut TaskContext)
                .write(crate::arch::user_context(entry, user_rsp, arg, tcb.kernel_stack_top));
        }
        tcb.user = true;
        tcb
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
            on_cpu: Some(cpu),
            inbox: VecDeque::new(),
            reply: None,
            wait_for: 0,
            wake_tick: 0,
            user: false,
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

/// Never runs: `new_user` overwrites the RIP it seeds
extern "C" fn user_entry_placeholder() -> ! {
    unreachable!("user task entered through its kernel placeholder")
}

/// The default initial entry point for tasks.
/// This calls the user's entry function and, if it returns, marks the task as exited.
pub extern "C" fn task_wrapper(entry: extern "C" fn() -> !) -> ! {
    entry()
}

/// Current task exit — called when a task function returns or voluntarily
/// exits. Never returns: the task is marked Exited (waking anyone blocked
/// on it) and yields; the scheduler never picks an Exited task again.
pub fn exit_current() -> ! {
    crate::scheduler::mark_current_exited();
    loop {
        crate::scheduler::yield_now();
    }
}
