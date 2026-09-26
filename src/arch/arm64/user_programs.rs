//! arm64 user programs: position-independent EL0 code (PC-relative data
//! via `adr`), assembled into the kernel image and *copied* into user
//! pages by `memory::user::load_program`. They reach the kernel only
//! through `svc #0` (x8 = syscall number, x0-x4 = arguments, result in
//! x0; numbers from `crate::syscall`). Same programs, same behavior as
//! the amd64 set.

use core::arch::global_asm;

global_asm!(
    r#"
    .pushsection .rodata.user_programs, "a"

    // SYS_LOG of the string between two labels
    .macro LOG start, end
    adr x0, \start
    adr x1, \end
    sub x1, x1, x0
    mov x8, #1
    svc #0
    .endm

    .macro EXIT
    mov x8, #0
    svc #0
    .endm

    // ---------------------------------------------------------------
    // hello: greet, look up the kernel's "sysinfo" service, send it an
    // SVC_SYS_INFO request, print the reply, exit.
    // ---------------------------------------------------------------
    .balign 16
    .globl user_hello_start
user_hello_start:
    LOG uh_msg, uh_msg_end
    adr x0, uh_svc                       // SYS_LOOKUP_SERVICE
    adr x1, uh_svc_end
    sub x1, x1, x0
    mov x8, #7
    svc #0
    cbz x0, uh_exit
    mov x9, #0xFFFF                      // error codes are huge
    cmp x0, x9
    b.hi uh_exit
    mov x19, x0                          // service task ID

    sub sp, sp, #96                      // RQB on the user stack
    mov x2, sp
    mov x3, #12
uh_zero:
    str xzr, [x2], #8
    subs x3, x3, #1
    b.ne uh_zero
    mov w2, #1                           // service = SVC_SYS_INFO
    strh w2, [sp]

    mov x0, x19                          // SYS_SEND_RQB (blocks)
    mov x1, sp
    mov x8, #2
    svc #0
    cbnz x0, uh_exit                     // RqbStatus::Success == 0

    add x0, sp, #18                      // SYS_LOG(reply.data)
    ldrh w1, [sp, #12]
    mov x8, #1
    svc #0
uh_exit:
    EXIT
    brk #0
uh_msg: .ascii "Hello from EL0! Asking the kernel's sysinfo service over RQB IPC..."
uh_msg_end:
uh_svc: .ascii "sysinfo"
uh_svc_end:
    .balign 4
    .globl user_hello_end
user_hello_end:

    // ---------------------------------------------------------------
    // uecho: a user-mode service. Registers as "uecho", then upper-cases
    // each request's payload and replies. SVC_TEXT_SHUTDOWN (0x03FF)
    // makes it exit without replying.
    // ---------------------------------------------------------------
    .balign 16
    .globl user_echo_start
user_echo_start:
    adr x0, ue_name                      // SYS_REGISTER_SERVICE
    adr x1, ue_name_end
    sub x1, x1, x0
    mov x8, #8
    svc #0
    LOG ue_msg, ue_msg_end
    sub sp, sp, #96
ue_loop:
    mov x0, sp                           // SYS_RECEIVE_RQB (blocks)
    mov x8, #3
    svc #0
    cbnz x0, ue_loop
    ldrh w2, [sp]
    mov w3, #0x03FF
    cmp w2, w3
    b.eq ue_exit
    ldrh w4, [sp, #12]                   // upper-case data[0..data_size]
    add x5, sp, #18
ue_up:
    cbz w4, ue_reply
    ldrb w6, [x5]
    sub w7, w6, #0x61
    cmp w7, #25
    b.hi ue_next
    sub w6, w6, #0x20
    strb w6, [x5]
ue_next:
    add x5, x5, #1
    sub w4, w4, #1
    b ue_up
ue_reply:
    strh wzr, [sp, #2]                   // status = Success
    ldr w0, [sp, #4]                     // SYS_REPLY_RQB(sender, rqb)
    mov x1, sp
    mov x8, #4
    svc #0
    b ue_loop
ue_exit:
    LOG ue_bye, ue_bye_end
    EXIT
    brk #0
ue_name: .ascii "uecho"
ue_name_end:
ue_msg: .ascii "uecho: user-mode service registered, serving requests"
ue_msg_end:
ue_bye: .ascii "uecho: shutdown request received, exiting without reply"
ue_bye_end:
    .balign 4
    .globl user_echo_end
user_echo_end:

    // ---------------------------------------------------------------
    // spinner: pure EL0 computation with no syscalls between rounds.
    // Only the timer can take the CPU away from it.
    // ---------------------------------------------------------------
    .balign 16
    .globl user_spin_start
user_spin_start:
    mov x19, #3
us_round:
    movz x9, #0xD180                     // 150,000,000 iterations
    movk x9, #0x08F0, lsl #16
us_spin:
    subs x9, x9, #1
    b.ne us_spin
    LOG us_msg, us_msg_end
    subs x19, x19, #1
    b.ne us_round
    EXIT
    brk #0
us_msg: .ascii "spinner: 150M-iteration EL0 loop done (preempted, never yielded)"
us_msg_end:
    .balign 4
    .globl user_spin_end
user_spin_end:

    // ---------------------------------------------------------------
    // rogue_read: x0 = a kernel address. Try to read it.
    // ---------------------------------------------------------------
    .balign 16
    .globl user_rogue_read_start
user_rogue_read_start:
    mov x19, x0
    LOG rr_msg, rr_msg_end
    ldr x0, [x19]                        // EL1-only page -> data abort
    LOG rr_bad, rr_bad_end
    EXIT
rr_msg: .ascii "rogue_read: reading kernel memory..."
rr_msg_end:
rr_bad: .ascii "rogue_read: READ KERNEL MEMORY (protection failure!)"
rr_bad_end:
    .balign 4
    .globl user_rogue_read_end
user_rogue_read_end:

    // ---------------------------------------------------------------
    // rogue_priv: mask interrupts from EL0 (the arm64 `cli`).
    // ---------------------------------------------------------------
    .balign 16
    .globl user_rogue_priv_start
user_rogue_priv_start:
    LOG rp_msg, rp_msg_end
    msr daifset, #2                      // SCTLR_EL1.UMA=0 -> trapped
    LOG rp_bad, rp_bad_end
    EXIT
rp_msg: .ascii "rogue_priv: executing msr daifset..."
rp_msg_end:
rp_bad: .ascii "rogue_priv: DISABLED INTERRUPTS (protection failure!)"
rp_bad_end:
    .balign 4
    .globl user_rogue_priv_end
user_rogue_priv_end:

    // ---------------------------------------------------------------
    // rogue_ptr: x0 = a kernel address. Ask the kernel to print from it;
    // a correct kernel refuses with EFAULT (-1).
    // ---------------------------------------------------------------
    .balign 16
    .globl user_rogue_ptr_start
user_rogue_ptr_start:
    mov x1, #16                          // SYS_LOG(kernel_ptr, 16)
    mov x8, #1
    svc #0
    cmn x0, #1
    b.ne rq_leaked
    LOG rq_ok, rq_ok_end
    EXIT
rq_leaked:
    LOG rq_bad, rq_bad_end
    EXIT
rq_ok: .ascii "rogue_ptr: kernel refused a kernel pointer with EFAULT"
rq_ok_end:
rq_bad: .ascii "rogue_ptr: KERNEL ACCEPTED A KERNEL POINTER (protection failure!)"
rq_bad_end:
    .balign 4
    .globl user_rogue_ptr_end
user_rogue_ptr_end:

    .popsection
    "#
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

macro_rules! program {
    ($name:ident, $start:ident, $end:ident) => {
        pub fn $name() -> &'static [u8] {
            blob(core::ptr::addr_of!($start), core::ptr::addr_of!($end))
        }
    };
}

program!(hello, user_hello_start, user_hello_end);
program!(echo, user_echo_start, user_echo_end);
program!(spinner, user_spin_start, user_spin_end);
program!(rogue_read, user_rogue_read_start, user_rogue_read_end);
program!(rogue_priv, user_rogue_priv_start, user_rogue_priv_end);
program!(rogue_ptr, user_rogue_ptr_start, user_rogue_ptr_end);
