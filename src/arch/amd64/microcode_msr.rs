//! Boot-only recoverable MSR access. Exact instruction fixups; no allocation.
core::arch::global_asm!(r#"
.text
.global ucode_rdmsr, ucode_wrmsr
ucode_rdmsr:
    movl %edi, %ecx
1:  rdmsr
    shlq $32, %rdx
    orq %rdx, %rax
    movq %rax, (%rsi)
    xorl %eax, %eax
    jmp __x86_return_thunk
    int3
2:  movl $1, %eax
    jmp __x86_return_thunk
    int3
ucode_wrmsr:
    movl %edi, %ecx
    movl %esi, %eax
    movq %rsi, %rdx
    shrq $32, %rdx
3:  wrmsr
    xorl %eax, %eax
    jmp __x86_return_thunk
    int3
4:  movl $1, %eax
    jmp __x86_return_thunk
    int3
.global ucode_gp_probe
ucode_gp_probe:
    movw $0xfff8, %ax
5:  movw %ax, %ds
    xorl %eax, %eax
    jmp __x86_return_thunk
    int3
6:  movl $1, %eax
    jmp __x86_return_thunk
    int3
.pushsection .rodata.ucode_ex_table, "a"
.balign 8
.global ucode_ex_table
ucode_ex_table:
    .quad 1b - ., 2b - ., 3b - ., 4b - ., 5b - ., 6b - .
.popsection
"#, options(att_syntax));
extern "C" {
    fn ucode_gp_probe() -> u64;
    fn ucode_rdmsr(msr: u32, output: *mut u64) -> u64;
    fn ucode_wrmsr(msr: u32, value: u64) -> u64;
    static ucode_ex_table: u8;
}
pub fn read(msr: u32) -> Option<u64> {
    let mut value = 0;
    (unsafe { ucode_rdmsr(msr, &mut value) } == 0).then_some(value)
}
pub fn write(msr: u32, value: u64) -> bool { unsafe { ucode_wrmsr(msr, value) == 0 } }
pub fn fixup(pc: u64) -> Option<u64> {
    let entries = core::ptr::addr_of!(ucode_ex_table).cast::<i64>();
    for i in [0, 2, 4] {
        unsafe {
            let from = entries.add(i); let to = from.add(1);
            if pc == (from as u64).wrapping_add_signed(from.read()) {
                return Some((to as u64).wrapping_add_signed(to.read()));
            }
        }
    }
    None
}

pub fn self_test() {
    assert_eq!(unsafe { ucode_gp_probe() }, 1, "boot #GP fixup gate failed");
}
