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
    record_kill(scheduler::current_task_id());
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

/// Recently fault-killed task IDs (so checks can tell "killed" from
/// "exited on its own")
static KILLED: [AtomicU32; 16] = [const { AtomicU32::new(0) }; 16];
static KILLED_NEXT: AtomicU32 = AtomicU32::new(0);

fn record_kill(tid: u32) {
    let i = KILLED_NEXT.fetch_add(1, Ordering::Relaxed) as usize % KILLED.len();
    KILLED[i].store(tid, Ordering::Relaxed);
}

/// Whether task `tid` was killed by a fault
pub fn was_killed(tid: u32) -> bool {
    tid != 0 && KILLED.iter().any(|k| k.load(Ordering::Relaxed) == tid)
}

// ========================================================================
// Loading
// ========================================================================

static TID_HELLO: AtomicU32 = AtomicU32::new(0);
static TID_SPIN: AtomicU32 = AtomicU32::new(0);
static TID_ROGUE_READ: AtomicU32 = AtomicU32::new(0);
static TID_ROGUE_PRIV: AtomicU32 = AtomicU32::new(0);
static TID_ROGUE_PTR: AtomicU32 = AtomicU32::new(0);
static TID_ROGUE_PEEK: AtomicU32 = AtomicU32::new(0);
/// ELF programs loaded from /BIN on the exFAT disk
static ELF_TIDS: [AtomicU32; 8] = [const { AtomicU32::new(0) }; 8];

/// Load a program into its own address space and start it as a user-mode
/// task; returns (task ID, entry address)
fn spawn_at(name: &'static str, code: &'static [u8], arg: u64) -> (u32, u64) {
    match crate::memory::user::load_program(code) {
        Ok(img) => (
            scheduler::create_user_task(img.entry, img.stack_top, arg, img.space, img.slot, name),
            img.entry,
        ),
        Err(e) => {
            crate::serial::write_str("[USER] failed to load ");
            crate::serial::write_str(name);
            crate::serial::write_str(": ");
            crate::serial::write_line(e);
            (0, 0)
        }
    }
}

fn spawn(name: &'static str, code: &'static [u8], arg: u64) -> u32 {
    spawn_at(name, code, arg).0
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
    let (_, uecho_code) = spawn_at("uecho", prog::echo(), 0);
    TID_HELLO.store(spawn("hello", prog::hello(), 0), Ordering::SeqCst);
    TID_SPIN.store(spawn("spinner", prog::spinner(), 0), Ordering::SeqCst);
    TID_ROGUE_READ.store(spawn("rogue_read", prog::rogue_read(), kernel_addr), Ordering::SeqCst);
    TID_ROGUE_PRIV.store(spawn("rogue_priv", prog::rogue_priv(), 0), Ordering::SeqCst);
    TID_ROGUE_PTR.store(spawn("rogue_ptr", prog::rogue_ptr(), kernel_addr), Ordering::SeqCst);
    // The same reader, aimed at another program's code: with per-task
    // address spaces those pages simply aren't mapped for it
    TID_ROGUE_PEEK.store(spawn("rogue_peek", prog::rogue_read(), uecho_code), Ordering::SeqCst);

    spawn_elf_programs();
}

/// Load every `*.ELF` in /BIN on the exFAT disk and run it as a user task
fn spawn_elf_programs() {
    use alloc::string::String;
    let entries = match crate::fs::exfat::read_dir("BIN") {
        Some(e) => e,
        None => {
            crate::serial::write_line("[USER] No /BIN directory on disk — no ELF programs to run");
            return;
        }
    };
    let mut n = 0;
    for (name, is_dir, size) in entries {
        if is_dir || !name.to_ascii_uppercase().ends_with(".ELF") || n >= ELF_TIDS.len() {
            continue;
        }
        let mut path = String::from("BIN/");
        path.push_str(&name);
        let mut line: heapless::String<128> = heapless::String::new();
        let tid = match crate::fs::exfat::read_file(&path)
            .ok_or("read failed")
            .and_then(|img| crate::memory::user::load_elf(&img))
        {
            Ok(img) => {
                // Task names live forever; ELF names come from disk
                let task_name: &'static str = alloc::boxed::Box::leak(name.clone().into_boxed_str());
                let tid = scheduler::create_user_task(img.entry, img.stack_top, 0, img.space, img.slot, task_name);
                let _ = write!(line, "[USER] Loaded /{} ({} bytes) as T{}, entry 0x{:x}\n", path, size, tid, img.entry);
                tid
            }
            Err(e) => {
                let _ = write!(line, "[USER] /{}: not loaded: {}\n", path, e);
                0
            }
        };
        crate::serial::write_str(&line);
        if tid != 0 {
            ELF_TIDS[n].store(tid, Ordering::SeqCst);
            n += 1;
        }
    }
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

/// Whether the task has exited (or already been reaped and forgotten)
fn exited(tid: &AtomicU32) -> bool {
    let tid = tid.load(Ordering::SeqCst);
    tid != 0 && matches!(scheduler::task_state(tid), Some(TaskState::Exited) | None)
}

fn killed(tid: &AtomicU32) -> bool {
    was_killed(tid.load(Ordering::SeqCst))
}

/// Build a minimal one-segment ELF image for loader tests
fn test_elf(machine: u16, vaddr: u64, flags: u32, entry: u64) -> alloc::vec::Vec<u8> {
    let mut img = alloc::vec![0u8; 64 + 56 + 16];
    img[0..4].copy_from_slice(b"\x7fELF");
    img[4] = 2; // 64-bit
    img[5] = 1; // little-endian
    img[6] = 1;
    img[16..18].copy_from_slice(&2u16.to_le_bytes()); // ET_EXEC
    img[18..20].copy_from_slice(&machine.to_le_bytes());
    img[24..32].copy_from_slice(&entry.to_le_bytes());
    img[32..40].copy_from_slice(&64u64.to_le_bytes()); // phoff
    img[54..56].copy_from_slice(&56u16.to_le_bytes()); // phentsize
    img[56..58].copy_from_slice(&1u16.to_le_bytes()); // phnum
    let ph = 64;
    img[ph..ph + 4].copy_from_slice(&1u32.to_le_bytes()); // PT_LOAD
    img[ph + 4..ph + 8].copy_from_slice(&flags.to_le_bytes());
    img[ph + 8..ph + 16].copy_from_slice(&120u64.to_le_bytes()); // offset
    img[ph + 16..ph + 24].copy_from_slice(&vaddr.to_le_bytes());
    img[ph + 32..ph + 40].copy_from_slice(&16u64.to_le_bytes()); // filesz
    img[ph + 40..ph + 48].copy_from_slice(&16u64.to_le_bytes()); // memsz
    img
}

/// The ELF loader must accept a well-formed image and reject malformed
/// ones (images come from disk: untrusted input)
fn elf_loader_tests() -> (usize, usize) {
    use crate::memory::user::check_elf;
    let base = crate::memory::user::USER_BASE + 0x300_0000;
    let m = crate::arch::ELF_MACHINE;
    let good = check_elf(&test_elf(m, base, 5, base)).is_ok();
    let bad = [
        check_elf(b"definitely not an ELF image, just some bytes of text......................"),
        check_elf(&test_elf(m ^ 0x1, base, 5, base)), // other architecture
        check_elf(&test_elf(m, 0xFFFF_8000_0000_0000, 5, 0xFFFF_8000_0000_0000)), // kernel space
        check_elf(&test_elf(m, base, 7, base)), // writable + executable
        check_elf(&test_elf(m, base, 4, base)), // entry not executable
    ];
    let rejected = bad.iter().filter(|r| r.is_err()).count();
    (good as usize, rejected)
}

/// Churn: run short-lived tasks in waves and verify every frame they used
/// comes back once the reaper has run. Heap growth also consumes frames,
/// so it is accounted for separately. Returns (tasks, frames reclaimed,
/// leaked frames).
fn churn_test() -> (usize, u64, i64) {
    use crate::memory::heap::{free_frames, grown_frames};
    const WAVES: usize = 2;
    const PER_WAVE: usize = 8;

    wait_until(3000, || scheduler::unreaped_tasks() == 0);
    let free_before = free_frames() as i64;
    let heap_before = grown_frames() as i64;
    let (_, reaped_before) = scheduler::reaper_stats();

    let mut ran = 0;
    for _ in 0..WAVES {
        let mut tids = [const { AtomicU32::new(0) }; PER_WAVE];
        for t in tids.iter_mut() {
            *t.get_mut() = spawn("churn", crate::arch::user_programs::exit_only(), 0);
        }
        wait_until(5000, || tids.iter().all(exited));
        ran += tids.iter().filter(|t| t.load(Ordering::Relaxed) != 0).count();
    }
    wait_until(3000, || scheduler::unreaped_tasks() == 0);

    let heap_grew = grown_frames() as i64 - heap_before;
    let leaked = free_before - (free_frames() as i64 + heap_grew);
    let (_, reaped_after) = scheduler::reaper_stats();
    (ran, reaped_after - reaped_before, leaked)
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
            && exited(&TID_ROGUE_PEEK)
    });

    crate::serial::write_str("[USER] Userspace checks:\n");
    pass &= report("kernel -> user-mode service (3 round trips)", uecho != 0 && echoed == 3);
    pass &= report("hello finished", exited(&TID_HELLO));
    pass &= report("spinner preempted in user mode, finished", exited(&TID_SPIN));
    pass &= report("rogue_read killed, kernel alive", killed(&TID_ROGUE_READ));
    pass &= report("rogue_priv killed, kernel alive", killed(&TID_ROGUE_PRIV));
    pass &= report("rogue_ptr refused, exited", exited(&TID_ROGUE_PTR) && !killed(&TID_ROGUE_PTR));
    pass &= report("rogue_peek can't see uecho's pages", killed(&TID_ROGUE_PEEK));
    pass &= all_done;

    // A user-mode service exiting mid-request fails it with Aborted
    let mut rqb = Rqb::with_service(SVC_TEXT_SHUTDOWN);
    let status = scheduler::send_rqb(uecho, &mut rqb);
    pass &= report("user receiver exits -> sender gets Aborted", status == RqbStatus::Aborted);

    // ELF programs from the disk ran to completion (not killed)
    let elfs: heapless::Vec<u32, 8> = ELF_TIDS
        .iter()
        .map(|t| t.load(Ordering::SeqCst))
        .filter(|&t| t != 0)
        .collect();
    if !elfs.is_empty() {
        let done = wait_until(10_000, || {
            elfs.iter().all(|&t| matches!(scheduler::task_state(t), Some(TaskState::Exited) | None))
        });
        let clean = elfs.iter().all(|&t| !was_killed(t));
        let mut what: heapless::String<64> = heapless::String::new();
        let _ = write!(what, "{} ELF programs from /BIN ran to completion", elfs.len());
        pass &= report(&what, done && clean);
    }
    let (good, rejected) = elf_loader_tests();
    pass &= report("ELF loader: accepts valid, rejects 5 bad", good == 1 && rejected == 5);

    // Exited tasks' stacks and address spaces are reclaimed
    let (ran, frames, leaked) = churn_test();
    let mut what: heapless::String<64> = heapless::String::new();
    let _ = write!(what, "{} churn tasks reclaimed ({} frames)", ran, frames);
    pass &= report(&what, ran == 16 && frames > 0 && leaked <= 0);
    if leaked > 0 {
        let mut line: heapless::String<64> = heapless::String::new();
        let _ = write!(line, "[USER]     {} frames leaked\n", leaked);
        crate::serial::write_str(&line);
    }

    crate::serial::write_str(if pass {
        "[USER] ✓ All userspace checks passed\n"
    } else {
        "[USER] ✗ Userspace checks FAILED\n"
    });
    scheduler::exit_current();
}
