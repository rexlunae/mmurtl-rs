//! Userspace — user-mode programs and their boot-time demonstration.
//!
//! The programs are small position-independent flat binaries written in
//! each architecture's assembly (`crate::arch::user_programs`). They are
//! assembled into the kernel image, then *copied* into freshly mapped user
//! pages by `memory::user::load_program` — the kernel's own copy is never
//! mapped user-accessible. Each talks to the kernel only through the
//! syscall trap (`int 0x80` / `svc #0`). This module — loading, the
//! kernel-side service, and the checks — is architecture-neutral.
//!
//! | program     | what it demonstrates |
//! |-------------|----------------------|
//! | hello       | user-mode syscalls + RQB IPC to a kernel service |
//! | uecho       | a *user-mode* service: kernel tasks send it requests |
//! | spinner     | a user-mode busy loop is preempted like any task |
//! | rogue_read  | reading kernel memory → page fault → task killed, kernel lives |
//! | rogue_priv  | a privileged instruction (`cli` / `msr daifset`) → task killed |
//! | rogue_ptr   | handing the kernel a kernel pointer → EFAULT, not a leak |

use core::fmt::Write;
use core::sync::atomic::{AtomicU32, Ordering};

use crate::scheduler::{self, Rqb, RqbStatus, TaskState};
use crate::scheduler::{SVC_SYS_INFO, SVC_TEXT_SHUTDOWN};

// ========================================================================
// Faults
// ========================================================================

/// A fault in user code is the task's problem, not the kernel's: report
/// it, kill the task (waking anyone blocked on it), and switch away for
/// good. Called by the architecture's exception handlers. Never returns.
pub fn kill_faulting_task(what: &str, pc: u64, addr: Option<u64>) -> ! {
    let mut line: heapless::String<160> = heapless::String::new();
    let _ = write!(
        line,
        "[USER] T{} \"{}\" killed: {} at pc=0x{:x}",
        scheduler::current_task_id(),
        scheduler::current_task_name(),
        what,
        pc
    );
    if let Some(a) = addr {
        let _ = write!(line, ", addr=0x{:x}", a);
    }
    let _ = line.push('\n');
    crate::serial::write_str(&line);
    scheduler::exit_current();
}

// ========================================================================
// Loading
// ========================================================================

static TID_HELLO: AtomicU32 = AtomicU32::new(0);
static TID_SPIN: AtomicU32 = AtomicU32::new(0);
static TID_ROGUE_READ: AtomicU32 = AtomicU32::new(0);
static TID_ROGUE_PRIV: AtomicU32 = AtomicU32::new(0);
static TID_ROGUE_PTR: AtomicU32 = AtomicU32::new(0);

/// Load a program into user memory and start it as a user-mode task
fn spawn(name: &'static str, code: &'static [u8], arg: u64) -> u32 {
    match crate::memory::user::load_program(code) {
        Ok(img) => scheduler::create_user_task(img.entry, img.stack_top, arg, name),
        Err(e) => {
            crate::serial::write_str("[USER] failed to load ");
            crate::serial::write_str(name);
            crate::serial::write_str(": ");
            crate::serial::write_line(e);
            0
        }
    }
}

/// Start the kernel-side service and checker, then the user programs
pub fn demo() {
    if !crate::memory::user::init() {
        return;
    }
    // A kernel address for the rogue programs to aim at
    let kernel_addr = crate::VERSION.as_ptr() as u64;

    scheduler::create_task(sysinfo_service_task, scheduler::PRIORITY_DEFAULT, "sysinfo");
    scheduler::create_task(user_check_task, scheduler::PRIORITY_DEFAULT, "userchk");

    use crate::arch::user_programs as prog;
    spawn("uecho", prog::echo(), 0);
    TID_HELLO.store(spawn("hello", prog::hello(), 0), Ordering::SeqCst);
    TID_SPIN.store(spawn("spinner", prog::spinner(), 0), Ordering::SeqCst);
    TID_ROGUE_READ.store(spawn("rogue_read", prog::rogue_read(), kernel_addr), Ordering::SeqCst);
    TID_ROGUE_PRIV.store(spawn("rogue_priv", prog::rogue_priv(), 0), Ordering::SeqCst);
    TID_ROGUE_PTR.store(spawn("rogue_ptr", prog::rogue_ptr(), kernel_addr), Ordering::SeqCst);
}

// ========================================================================
// Kernel-side tasks
// ========================================================================

/// Kernel service answering SVC_SYS_INFO for user programs
extern "C" fn sysinfo_service_task() -> ! {
    crate::ipc::register_service("sysinfo", scheduler::current_task_id());
    loop {
        let req = scheduler::receive_rqb();
        let mut resp = Rqb::with_service(req.service);
        if req.service == SVC_SYS_INFO {
            let mut text: heapless::String<64> = heapless::String::new();
            let _ = write!(
                text,
                "{} v{}: {} CPUs, up {} ms",
                crate::KERNEL_NAME,
                crate::VERSION,
                crate::arch::smp::cpus_online(),
                scheduler::jiffies() * 1000 / scheduler::SCHEDULER_FREQUENCY_HZ as u64
            );
            resp.set_data(text.as_bytes());
            resp.set_status(RqbStatus::Success);
        } else {
            resp.set_status(RqbStatus::InvalidService);
        }
        scheduler::reply_rqb(req.sender_id, &resp);
    }
}

/// Poll `cond` every 20 ms for up to `ms`
fn wait_until(ms: u64, mut cond: impl FnMut() -> bool) -> bool {
    let mut waited = 0;
    while !cond() {
        if waited >= ms {
            return false;
        }
        scheduler::sleep_ms(20);
        waited += 20;
    }
    true
}

fn exited(tid: &AtomicU32) -> bool {
    scheduler::task_state(tid.load(Ordering::SeqCst)) == Some(TaskState::Exited)
}

/// Verifies the userspace demo from the kernel side
extern "C" fn user_check_task() -> ! {
    let mut pass = true;
    let report = |what: &str, ok: bool| -> bool {
        let mut line: heapless::String<128> = heapless::String::new();
        let _ = write!(line, "[USER]   {:<40} {}\n", what, if ok { "✓" } else { "✗" });
        crate::serial::write_str(&line);
        ok
    };

    // Kernel → user-mode service round trips
    let uecho = if wait_until(5000, || crate::ipc::lookup_service("uecho").is_some()) {
        crate::ipc::lookup_service("uecho").unwrap()
    } else {
        0
    };
    let mut echoed = 0;
    for i in 0..3 {
        let mut payload: heapless::String<32> = heapless::String::new();
        let _ = write!(payload, "kernel to user mode #{}", i);
        let mut rqb = Rqb::with_service(0x0400);
        rqb.set_data(payload.as_bytes());
        let status = scheduler::send_rqb(uecho, &mut rqb);
        let expect = payload.to_ascii_uppercase();
        if status == RqbStatus::Success && rqb.sender_id == uecho && rqb.get_data() == expect.as_bytes() {
            echoed += 1;
        }
    }

    // Wait for every program to finish (or be killed)
    let all_done = wait_until(10_000, || {
        exited(&TID_HELLO)
            && exited(&TID_SPIN)
            && exited(&TID_ROGUE_READ)
            && exited(&TID_ROGUE_PRIV)
            && exited(&TID_ROGUE_PTR)
    });

    crate::serial::write_str("[USER] Userspace checks:\n");
    pass &= report("kernel -> user-mode service (3 round trips)", uecho != 0 && echoed == 3);
    pass &= report("hello finished", exited(&TID_HELLO));
    pass &= report("spinner preempted in user mode, finished", exited(&TID_SPIN));
    pass &= report("rogue_read killed, kernel alive", exited(&TID_ROGUE_READ));
    pass &= report("rogue_priv killed, kernel alive", exited(&TID_ROGUE_PRIV));
    pass &= report("rogue_ptr refused, exited", exited(&TID_ROGUE_PTR));
    pass &= all_done;

    // A user-mode service exiting mid-request fails it with Aborted
    let mut rqb = Rqb::with_service(SVC_TEXT_SHUTDOWN);
    let status = scheduler::send_rqb(uecho, &mut rqb);
    pass &= report("user receiver exits -> sender gets Aborted", status == RqbStatus::Aborted);

    crate::serial::write_str(if pass {
        "[USER] ✓ All userspace checks passed\n"
    } else {
        "[USER] ✗ Userspace checks FAILED\n"
    });
    scheduler::exit_current();
}
