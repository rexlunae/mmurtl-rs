//! Compiler external indirect-branch thunks.
core::arch::global_asm!(r#"
.text
.macro INDIRECT_THUNK reg
.balign 64
.global __x86_indirect_thunk_\reg
.type __x86_indirect_thunk_\reg, @function
__x86_indirect_thunk_\reg:
    endbr64
    call .Linstall_target\@
.Lprediction_sink\@:
    pause
    lfence
    jmp .Lprediction_sink\@
.Linstall_target\@:
    mov [rsp], \reg
    ret
    int3
.size __x86_indirect_thunk_\reg, .-__x86_indirect_thunk_\reg
.endm
INDIRECT_THUNK rax
INDIRECT_THUNK rcx
INDIRECT_THUNK rdx
INDIRECT_THUNK rbx
INDIRECT_THUNK rbp
INDIRECT_THUNK rsi
INDIRECT_THUNK rdi
INDIRECT_THUNK r8
INDIRECT_THUNK r9
INDIRECT_THUNK r10
INDIRECT_THUNK r11
INDIRECT_THUNK r12
INDIRECT_THUNK r13
INDIRECT_THUNK r14
INDIRECT_THUNK r15
"#);
