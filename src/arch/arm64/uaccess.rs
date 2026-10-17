//! Unprivileged accesses enforce EL0 page permissions even from EL1.
//! Relative exception-table entries also work for relocated boot Images.
core::arch::global_asm!(r#"
    .text
    .globl user_copy_from
user_copy_from:
    cbz x2, .Lfrom_done
.Lfrom_fault:
    ldtrb w3, [x1]
    strb w3, [x0]
    add x1, x1, #1
    add x0, x0, #1
    subs x2, x2, #1
    b.ne .Lfrom_fault
.Lfrom_done:
    mov x0, x2
    ret
    .globl user_copy_to
user_copy_to:
    cbz x2, .Lto_done
.Lto_loop:
    ldrb w3, [x1]
.Lto_fault:
    sttrb w3, [x0]
    add x1, x1, #1
    add x0, x0, #1
    subs x2, x2, #1
    b.ne .Lto_loop
.Lto_done:
    mov x0, x2
    ret
    .pushsection .rodata.ex_table, "a"
    .balign 8
    .globl user_copy_ex_table
user_copy_ex_table:
    .quad .Lfrom_fault - .
    .quad .Lfrom_done - .
    .quad .Lto_fault - .
    .quad .Lto_done - .
    .popsection
"#);
extern "C" {
    fn user_copy_from(dst: *mut u8, src: u64, len: usize) -> usize;
    fn user_copy_to(dst: u64, src: *const u8, len: usize) -> usize;
    static user_copy_ex_table: [i64; 4];
}
pub unsafe fn copy_from_user(dst: *mut u8, src: u64, len: usize) -> usize {
    user_copy_from(dst, src, len)
}
pub unsafe fn copy_to_user(dst: u64, src: *const u8, len: usize) -> usize {
    user_copy_to(dst, src, len)
}
pub fn fixup(pc: u64) -> Option<u64> {
    unsafe {
        let table = core::ptr::addr_of!(user_copy_ex_table).cast::<i64>();
        for n in [0, 2] {
            let entry = table.add(n);
            if pc == (entry as u64).wrapping_add_signed(entry.read()) {
                return Some((entry.add(1) as u64).wrapping_add_signed(entry.add(1).read()));
            }
        }
        None
    }
}

pub use super::mmu::remove_test_page;
