//! amd64 user programs: position-independent ring-3 code (RIP-relative
//! data only), assembled into the kernel image and *copied* into user
//! pages by `memory::user::load_program`. They reach the kernel only
//! through `int 0x80` (syscall numbers from `crate::syscall`).

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
