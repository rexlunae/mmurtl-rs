//! IPC — Inter-Process Communication via message passing.
//!
//! MMURTL's core design is built on RQBs (Request Blocks) passed
//! synchronously between tasks. The blocking primitives live in the
//! scheduler (`send_rqb` / `receive_rqb` / `reply_rqb`), because they are
//! task-state transitions; this module adds the service name registry and
//! the boot-time IPC demonstration.
//!
//! Semantics (MMURTL Request/Respond):
//!   - `send_rqb(tid, &mut rqb)` queues a copy of the request in the
//!     receiver's inbox and blocks the sender (WaitingReply) until the
//!     receiver replies. No CPU is consumed while blocked.
//!   - `receive_rqb()` pops the oldest pending request, blocking
//!     (WaitingRqb) while the inbox is empty.
//!   - `reply_rqb(sender, &rqb)` copies the reply into the blocked sender's
//!     reply slot and makes it Ready.
//!   - A receiver that exits fails every request it still owes with
//!     `Aborted`; sending to a dead or unknown task fails with `NotFound`.

use alloc::vec::Vec;
use core::fmt::Write;
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;

use crate::scheduler::{self, Rqb, RqbStatus};
use crate::scheduler::{SVC_TEXT_REVERSE, SVC_TEXT_SHUTDOWN, SVC_TEXT_UPPER};

// ========================================================================
// Service registry — MMURTL-style named services
// ========================================================================

const SERVICE_NAME_MAX: usize = 16;

static SERVICES: Mutex<Vec<(heapless::String<SERVICE_NAME_MAX>, u32)>> = Mutex::new(Vec::new());

fn with_services<R>(f: impl FnOnce(&mut Vec<(heapless::String<SERVICE_NAME_MAX>, u32)>) -> R) -> R {
    x86_64::instructions::interrupts::without_interrupts(|| f(&mut SERVICES.lock()))
}

/// Register `tid` under `name` (replacing any previous owner of the name)
pub fn register_service(name: &str, tid: u32) -> bool {
    let mut key: heapless::String<SERVICE_NAME_MAX> = heapless::String::new();
    if name.is_empty() || key.push_str(name).is_err() {
        return false;
    }
    with_services(|s| {
        s.retain(|(n, _)| n.as_str() != name);
        s.push((key, tid));
    });
    true
}

/// Look up the task serving `name`
pub fn lookup_service(name: &str) -> Option<u32> {
    with_services(|s| s.iter().find(|(n, _)| n.as_str() == name).map(|&(_, t)| t))
}

/// Initialize the IPC subsystem
pub fn init() {
    crate::serial::write_line("[IPC] RQB IPC ready: blocking send/receive/reply, service registry");
}

// ========================================================================
// Boot demo: a text service, concurrent clients, and error-path checks
// ========================================================================

const CLIENTS: usize = 3;
const REQUESTS_PER_CLIENT: usize = 5;

static CLIENTS_DONE: AtomicUsize = AtomicUsize::new(0);
static CLIENT_FAILURES: AtomicUsize = AtomicUsize::new(0);

/// Spawn the IPC demo tasks
pub fn demo() {
    scheduler::create_task(text_service_task, scheduler::PRIORITY_DEFAULT, "textsvc");
    for _ in 0..CLIENTS {
        scheduler::create_task(text_client_task, scheduler::PRIORITY_DEFAULT, "client");
    }
    scheduler::create_task(ipc_check_task, scheduler::PRIORITY_DEFAULT, "ipccheck");
}

fn log(line: &str) {
    crate::serial::write_str(line);
}

/// Text service: upper-cases or reverses payloads. Blocks in receive
/// between requests, so it costs no CPU while idle.
extern "C" fn text_service_task() -> ! {
    let me = scheduler::current_task_id();
    register_service("text", me);

    let mut served = 0u64;
    loop {
        let req = scheduler::receive_rqb();
        let mut resp = Rqb::with_service(req.service);
        match req.service {
            SVC_TEXT_UPPER => {
                let mut buf = [0u8; scheduler::RQB_DATA_SIZE];
                let d = req.get_data();
                for (o, i) in buf.iter_mut().zip(d) {
                    *o = i.to_ascii_uppercase();
                }
                resp.set_data(&buf[..d.len()]);
                resp.set_status(RqbStatus::Success);
            }
            SVC_TEXT_REVERSE => {
                let mut buf = [0u8; scheduler::RQB_DATA_SIZE];
                let d = req.get_data();
                for (o, i) in buf.iter_mut().zip(d.iter().rev()) {
                    *o = *i;
                }
                resp.set_data(&buf[..d.len()]);
                resp.set_status(RqbStatus::Success);
            }
            SVC_TEXT_SHUTDOWN => {
                // Exit WITHOUT replying: the kernel must fail this request
                // (and anything still queued) with Aborted.
                let mut line: heapless::String<96> = heapless::String::new();
                let _ = write!(line, "[IPC] textsvc: served {} requests, shutting down\n", served);
                log(&line);
                register_service("text", 0);
                scheduler::exit_current();
            }
            _ => resp.set_status(RqbStatus::InvalidService),
        }
        scheduler::reply_rqb(req.sender_id, &resp);
        served += 1;
    }
}

/// Wait for a service to register (tasks start in arbitrary order)
fn wait_for_service(name: &str) -> u32 {
    loop {
        match lookup_service(name) {
            Some(tid) if tid != 0 => return tid,
            _ => scheduler::sleep_ms(10),
        }
    }
}

/// Client: fires requests at the text service and verifies every reply
extern "C" fn text_client_task() -> ! {
    let me = scheduler::current_task_id();
    let svc = wait_for_service("text");

    let mut ok = 0usize;
    let mut last: heapless::String<64> = heapless::String::new();
    for i in 0..REQUESTS_PER_CLIENT {
        let mut payload: heapless::String<48> = heapless::String::new();
        let _ = write!(payload, "t{} msg {}", me, i);

        let upper = i % 2 == 0;
        let mut rqb = Rqb::with_service(if upper { SVC_TEXT_UPPER } else { SVC_TEXT_REVERSE });
        rqb.set_data(payload.as_bytes());

        let status = scheduler::send_rqb(svc, &mut rqb);

        let mut expect: heapless::String<48> = heapless::String::new();
        if upper {
            for c in payload.chars() {
                let _ = expect.push(c.to_ascii_uppercase());
            }
        } else {
            for c in payload.chars().rev() {
                let _ = expect.push(c);
            }
        }
        if status == RqbStatus::Success && rqb.sender_id == svc && rqb.get_data() == expect.as_bytes() {
            ok += 1;
            last.clear();
            let _ = last.push_str(core::str::from_utf8(rqb.get_data()).unwrap_or("?"));
        }
    }

    let mut line: heapless::String<128> = heapless::String::new();
    let _ = write!(
        line,
        "[IPC] client T{} (CPU{}): {}/{} round trips verified, last reply \"{}\"\n",
        me,
        scheduler::current_cpu(),
        ok,
        REQUESTS_PER_CLIENT,
        last
    );
    log(&line);
    if ok != REQUESTS_PER_CLIENT {
        CLIENT_FAILURES.fetch_add(1, Ordering::SeqCst);
    }
    CLIENTS_DONE.fetch_add(1, Ordering::SeqCst);
    scheduler::exit_current();
}

/// Checks error paths and blocking behavior once the clients finish
extern "C" fn ipc_check_task() -> ! {
    let me = scheduler::current_task_id();
    while CLIENTS_DONE.load(Ordering::SeqCst) < CLIENTS {
        scheduler::sleep_ms(20);
    }
    let svc = wait_for_service("text");

    let mut pass = CLIENT_FAILURES.load(Ordering::SeqCst) == 0;
    let check = |what: &str, got: RqbStatus, want: RqbStatus| -> bool {
        let mut line: heapless::String<128> = heapless::String::new();
        let good = got == want;
        let _ = write!(
            line,
            "[IPC]   {:<34} -> {:?} {}\n",
            what,
            got,
            if good { "✓" } else { "✗" }
        );
        log(&line);
        good
    };

    log("[IPC] Error-path checks:\n");
    let mut r = Rqb::with_service(SVC_TEXT_UPPER);
    pass &= check("send to self", scheduler::send_rqb(me, &mut r), RqbStatus::InvalidParam);
    let mut r = Rqb::with_service(SVC_TEXT_UPPER);
    pass &= check("send to unknown task", scheduler::send_rqb(0xDEAD, &mut r), RqbStatus::NotFound);
    let mut r = Rqb::with_service(0x7777);
    pass &= check("unknown service code", scheduler::send_rqb(svc, &mut r), RqbStatus::InvalidService);
    pass &= check("reply to a non-waiting task", scheduler::reply_rqb(svc, &Rqb::new()), RqbStatus::InvalidParam);

    // Sleep is a real block: measure it against the system clock
    let t0 = scheduler::jiffies();
    scheduler::sleep_ms(250);
    let slept_ms = (scheduler::jiffies() - t0) * 1000 / scheduler::SCHEDULER_FREQUENCY_HZ as u64;
    let sleep_ok = (250..=400).contains(&slept_ms);
    let mut line: heapless::String<96> = heapless::String::new();
    let _ = write!(line, "[IPC]   {:<34} -> {} ms {}\n", "sleep_ms(250)", slept_ms, if sleep_ok { "✓" } else { "✗" });
    log(&line);
    pass &= sleep_ok;

    // Receiver exits while we're blocked on it → Aborted, then NotFound
    let mut r = Rqb::with_service(SVC_TEXT_SHUTDOWN);
    pass &= check("receiver exits before replying", scheduler::send_rqb(svc, &mut r), RqbStatus::Aborted);
    let mut r = Rqb::with_service(SVC_TEXT_UPPER);
    pass &= check("send to exited task", scheduler::send_rqb(svc, &mut r), RqbStatus::NotFound);

    log(if pass {
        "[IPC] ✓ All IPC checks passed\n"
    } else {
        "[IPC] ✗ IPC checks FAILED\n"
    });
    scheduler::exit_current();
}
