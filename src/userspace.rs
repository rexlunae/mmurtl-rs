//! Userspace — ring-3 programs and their boot-time demonstration.
//!
//! The programs are small position-independent flat binaries written in
//! assembly (RIP-relative data only). They are assembled into the kernel
//! image, then *copied* into freshly mapped user pages by
//! `memory::user::load_program` — the kernel's own copy is never mapped
//! user-accessible. Each talks to the kernel only through `int 0x80`.
//!
//! | program     | what it demonstrates |
//! |-------------|----------------------|
//! | hello       | ring-3 syscalls + RQB IPC to a kernel service |
//! | uecho       | a *user-mode* service: kernel tasks send it requests |
//! | spinner     | a ring-3 busy loop is preempted like any task |
//! | rogue_read  | reading kernel memory → #PF → task killed, kernel lives |
//! | rogue_priv  | `cli` in ring 3 → #GP → task killed |
//! | rogue_ptr   | handing the kernel a kernel pointer → EFAULT, not a leak |

use core::fmt::Write;
use core::sync::atomic::{AtomicU32, Ordering};

use crate::scheduler::{self, Rqb, RqbStatus, TaskState};
use crate::scheduler::{SVC_SYS_INFO, SVC_TEXT_SHUTDOWN};

// ========================================================================
// Ring-3 programs (AT&T syntax; syscall numbers from `syscall.rs`)
// ========================================================================

core::arch::global_asm!(
    r#"
    .pushsection .rodata.user_programs, "a"

    # ---------------------------------------------------------------
    # hello: greet, look up the kernel's "sysinfo" service, send it an
    # SVC_SYS_INFO request, print the reply, exit.
    # ---------------------------------------------------------------
    .globl user_hello_start
user_hello_start:
    movl $1, %eax                          # SYS_LOG
    leaq uh_msg(%rip), %rdi
    movl $(uh_msg_end - uh_msg), %esi
    int $0x80

    movl $7, %eax                          # SYS_LOOKUP_SERVICE
    leaq uh_svc(%rip), %rdi
    movl $(uh_svc_end - uh_svc), %esi
    int $0x80
    testq %rax, %rax
    jz uh_exit
    cmpq $0xFFFF, %rax                     # error codes are huge
    ja uh_exit
    movq %rax, %rbx                        # service task ID

    subq $96, %rsp                         # RQB on the user stack
    movq %rsp, %rdi
    xorl %eax, %eax
    movl $12, %ecx
    rep stosq
    movw $0x0001, (%rsp)                   # service = SVC_SYS_INFO

    movl $2, %eax                          # SYS_SEND_RQB (blocks)
    movq %rbx, %rdi
    movq %rsp, %rsi
    int $0x80
    testq %rax, %rax                       # RqbStatus::Success == 0
    jnz uh_exit

    movl $1, %eax                          # SYS_LOG(reply.data)
    leaq 18(%rsp), %rdi
    movzwl 12(%rsp), %esi
    int $0x80
uh_exit:
    xorl %eax, %eax                        # SYS_EXIT
    int $0x80
    ud2
uh_msg: .ascii "Hello from ring 3! Asking the kernel's sysinfo service over RQB IPC..."
uh_msg_end:
uh_svc: .ascii "sysinfo"
uh_svc_end:
    .globl user_hello_end
user_hello_end:

    # ---------------------------------------------------------------
    # uecho: a ring-3 service. Registers as "uecho", then upper-cases
    # each request's payload and replies. SVC_TEXT_SHUTDOWN (0x03FF)
    # makes it exit without replying.
    # ---------------------------------------------------------------
    .globl user_echo_start
user_echo_start:
    movl $8, %eax                          # SYS_REGISTER_SERVICE
    leaq ue_name(%rip), %rdi
    movl $(ue_name_end - ue_name), %esi
    int $0x80
    movl $1, %eax
    leaq ue_msg(%rip), %rdi
    movl $(ue_msg_end - ue_msg), %esi
    int $0x80
    subq $96, %rsp
ue_loop:
    movl $3, %eax                          # SYS_RECEIVE_RQB (blocks)
    movq %rsp, %rdi
    int $0x80
    testq %rax, %rax
    jnz ue_loop
    cmpw $0x03FF, (%rsp)
    je ue_exit
    movzwl 12(%rsp), %ecx                  # upper-case data[0..data_size]
    leaq 18(%rsp), %rdx
ue_up:
    testl %ecx, %ecx
    jz ue_reply
    movb (%rdx), %al
    cmpb $0x61, %al
    jb ue_next
    cmpb $0x7a, %al
    ja ue_next
    subb $0x20, %al
    movb %al, (%rdx)
ue_next:
    incq %rdx
    decl %ecx
    jmp ue_up
ue_reply:
    movw $0, 2(%rsp)                       # status = Success
    movl $4, %eax                          # SYS_REPLY_RQB(sender, rqb)
    movl 4(%rsp), %edi
    movq %rsp, %rsi
    int $0x80
    jmp ue_loop
ue_exit:
    movl $1, %eax
    leaq ue_bye(%rip), %rdi
    movl $(ue_bye_end - ue_bye), %esi
    int $0x80
    xorl %eax, %eax
    int $0x80
    ud2
ue_name: .ascii "uecho"
ue_name_end:
ue_msg: .ascii "uecho: user-mode service registered, serving requests"
ue_msg_end:
ue_bye: .ascii "uecho: shutdown request received, exiting without reply"
ue_bye_end:
    .globl user_echo_end
user_echo_end:

    # ---------------------------------------------------------------
    # spinner: ~0.5-1 s of pure ring-3 computation with no syscalls.
    # Only the timer can take the CPU away from it.
    # ---------------------------------------------------------------
    .globl user_spin_start
user_spin_start:
    movl $3, %r12d
us_round:
    movl $150000000, %ecx
us_spin:
    decl %ecx
    jnz us_spin
    movl $1, %eax
    leaq us_msg(%rip), %rdi
    movl $(us_msg_end - us_msg), %esi
    int $0x80
    decl %r12d
    jnz us_round
    xorl %eax, %eax
    int $0x80
    ud2
us_msg: .ascii "spinner: 150M-iteration ring-3 loop done (preempted, never yielded)"
us_msg_end:
    .globl user_spin_end
user_spin_end:

    # ---------------------------------------------------------------
    # rogue_read: RDI = a kernel address. Try to read it.
    # ---------------------------------------------------------------
    .globl user_rogue_read_start
user_rogue_read_start:
    movq %rdi, %rbx
    movl $1, %eax
    leaq rr_msg(%rip), %rdi
    movl $(rr_msg_end - rr_msg), %esi
    int $0x80
    movq (%rbx), %rax                      # supervisor page -> #PF
    movl $1, %eax
    leaq rr_bad(%rip), %rdi
    movl $(rr_bad_end - rr_bad), %esi
    int $0x80
    xorl %eax, %eax
    int $0x80
rr_msg: .ascii "rogue_read: reading kernel memory..."
rr_msg_end:
rr_bad: .ascii "rogue_read: READ KERNEL MEMORY (protection failure!)"
rr_bad_end:
    .globl user_rogue_read_end
user_rogue_read_end:

    # ---------------------------------------------------------------
    # rogue_priv: execute a privileged instruction.
    # ---------------------------------------------------------------
    .globl user_rogue_priv_start
user_rogue_priv_start:
    movl $1, %eax
    leaq rp_msg(%rip), %rdi
    movl $(rp_msg_end - rp_msg), %esi
    int $0x80
    cli                                    # CPL 3 > IOPL 0 -> #GP
    movl $1, %eax
    leaq rp_bad(%rip), %rdi
    movl $(rp_bad_end - rp_bad), %esi
    int $0x80
    xorl %eax, %eax
    int $0x80
rp_msg: .ascii "rogue_priv: executing cli..."
rp_msg_end:
rp_bad: .ascii "rogue_priv: DISABLED INTERRUPTS (protection failure!)"
rp_bad_end:
    .globl user_rogue_priv_end
user_rogue_priv_end:

    # ---------------------------------------------------------------
    # rogue_ptr: RDI = a kernel address. Ask the kernel to print from
    # it; a correct kernel refuses with EFAULT (-1).
    # ---------------------------------------------------------------
    .globl user_rogue_ptr_start
user_rogue_ptr_start:
    movq %rdi, %rbx
    movl $1, %eax                          # SYS_LOG(kernel_ptr, 16)
    movq %rbx, %rdi
    movl $16, %esi
    int $0x80
    cmpq $-1, %rax
    jne rq_leaked
    movl $1, %eax
    leaq rq_ok(%rip), %rdi
    movl $(rq_ok_end - rq_ok), %esi
    int $0x80
    xorl %eax, %eax
    int $0x80
rq_leaked:
    movl $1, %eax
    leaq rq_bad(%rip), %rdi
    movl $(rq_bad_end - rq_bad), %esi
    int $0x80
    xorl %eax, %eax
    int $0x80
rq_ok: .ascii "rogue_ptr: kernel refused a kernel pointer with EFAULT"
rq_ok_end:
rq_bad: .ascii "rogue_ptr: KERNEL ACCEPTED A KERNEL POINTER (protection failure!)"
rq_bad_end:
    .globl user_rogue_ptr_end
user_rogue_ptr_end:

    .popsection
    "#,
    options(att_syntax)
);

extern "C" {
    static user_hello_start: u8;
    static user_hello_end: u8;
    static user_echo_start: u8;
    static user_echo_end: u8;
    static user_spin_start: u8;
    static user_spin_end: u8;
    static user_rogue_read_start: u8;
    static user_rogue_read_end: u8;
    static user_rogue_priv_start: u8;
    static user_rogue_priv_end: u8;
    static user_rogue_ptr_start: u8;
    static user_rogue_ptr_end: u8;
}

/// The bytes between two assembly labels
fn blob(start: *const u8, end: *const u8) -> &'static [u8] {
    unsafe { core::slice::from_raw_parts(start, end as usize - start as usize) }
}

// ========================================================================
// Loading
// ========================================================================

static TID_HELLO: AtomicU32 = AtomicU32::new(0);
static TID_SPIN: AtomicU32 = AtomicU32::new(0);
static TID_ROGUE_READ: AtomicU32 = AtomicU32::new(0);
static TID_ROGUE_PRIV: AtomicU32 = AtomicU32::new(0);
static TID_ROGUE_PTR: AtomicU32 = AtomicU32::new(0);

/// Load a program into user memory and start it as a ring-3 task
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

/// Start the kernel-side service and checker, then the ring-3 programs
pub fn demo() {
    if !crate::memory::user::init() {
        return;
    }
    // A kernel address for the rogue programs to aim at
    let kernel_addr = crate::VERSION.as_ptr() as u64;

    scheduler::create_task(sysinfo_service_task, scheduler::PRIORITY_DEFAULT, "sysinfo");
    scheduler::create_task(user_check_task, scheduler::PRIORITY_DEFAULT, "userchk");

    unsafe {
        use core::ptr::addr_of;
        spawn("uecho", blob(addr_of!(user_echo_start), addr_of!(user_echo_end)), 0);
        TID_HELLO.store(
            spawn("hello", blob(addr_of!(user_hello_start), addr_of!(user_hello_end)), 0),
            Ordering::SeqCst,
        );
        TID_SPIN.store(
            spawn("spinner", blob(addr_of!(user_spin_start), addr_of!(user_spin_end)), 0),
            Ordering::SeqCst,
        );
        TID_ROGUE_READ.store(
            spawn(
                "rogue_read",
                blob(addr_of!(user_rogue_read_start), addr_of!(user_rogue_read_end)),
                kernel_addr,
            ),
            Ordering::SeqCst,
        );
        TID_ROGUE_PRIV.store(
            spawn(
                "rogue_priv",
                blob(addr_of!(user_rogue_priv_start), addr_of!(user_rogue_priv_end)),
                0,
            ),
            Ordering::SeqCst,
        );
        TID_ROGUE_PTR.store(
            spawn(
                "rogue_ptr",
                blob(addr_of!(user_rogue_ptr_start), addr_of!(user_rogue_ptr_end)),
                kernel_addr,
            ),
            Ordering::SeqCst,
        );
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
                crate::smp::cpus_online(),
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

    // Kernel → ring-3 service round trips
    let uecho = if wait_until(5000, || crate::ipc::lookup_service("uecho").is_some()) {
        crate::ipc::lookup_service("uecho").unwrap()
    } else {
        0
    };
    let mut echoed = 0;
    for i in 0..3 {
        let mut payload: heapless::String<32> = heapless::String::new();
        let _ = write!(payload, "kernel to ring 3 #{}", i);
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
    pass &= report("kernel -> ring-3 service (3 round trips)", uecho != 0 && echoed == 3);
    pass &= report("hello finished", exited(&TID_HELLO));
    pass &= report("spinner preempted in ring 3, finished", exited(&TID_SPIN));
    pass &= report("rogue_read killed, kernel alive", exited(&TID_ROGUE_READ));
    pass &= report("rogue_priv killed, kernel alive", exited(&TID_ROGUE_PRIV));
    pass &= report("rogue_ptr refused, exited", exited(&TID_ROGUE_PTR));
    pass &= all_done;

    // A ring-3 service exiting mid-request fails it with Aborted
    let mut rqb = Rqb::with_service(SVC_TEXT_SHUTDOWN);
    let status = scheduler::send_rqb(uecho, &mut rqb);
    pass &= report("ring-3 receiver exits -> sender gets Aborted", status == RqbStatus::Aborted);

    crate::serial::write_str(if pass {
        "[USER] ✓ All userspace checks passed\n"
    } else {
        "[USER] ✗ Userspace checks FAILED\n"
    });
    scheduler::exit_current();
}
