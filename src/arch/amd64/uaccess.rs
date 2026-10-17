//! The only kernel instruction allowed to recover a user-copy page fault.
//! Relative exception-table entries stay valid under kernel relocation.
core::arch::global_asm!(r#"
    .text
    .globl user_copy_bytes
user_copy_bytes:
    endbr64
    mov rcx, rdx
    cld
.Lcopy_fault:
    rep movsb
.Lcopy_fixup:
    mov rax, rcx
    jmp __x86_return_thunk
    int3
    .pushsection .rodata.ex_table, "a"
    .balign 8
    .globl user_copy_ex_table
user_copy_ex_table:
    .quad .Lcopy_fault - .
    .quad .Lcopy_fixup - .
    .popsection
"#);
extern "C" {
    fn user_copy_bytes(dst: *mut u8, src: *const u8, len: usize) -> usize;
    static user_copy_ex_table: [i64; 2];
}
/// Returns the bytes not copied. Buffers may be partially changed on failure.
pub unsafe fn copy_from_user(dst: *mut u8, src: u64, len: usize) -> usize {
    super::user_access_begin();
    let left = user_copy_bytes(dst, src as *const u8, len);
    super::user_access_end();
    left
}
pub unsafe fn copy_to_user(dst: u64, src: *const u8, len: usize) -> usize {
    copy_from_user(dst as *mut u8, src as u64, len)
}
pub fn fixup(pc: u64) -> Option<u64> {
    unsafe {
        let entry = core::ptr::addr_of!(user_copy_ex_table).cast::<i64>();
        let fault = (entry as u64).wrapping_add_signed(entry.read());
        let fixup = (entry.add(1) as u64).wrapping_add_signed(entry.add(1).read());
        (pc == fault).then_some(fixup)
    }
}

/// Boot self-test only: remove a page from the private, active test space.
/// Its frame remains owned by the caller until explicitly released.
pub unsafe fn remove_test_page(_root: u64, va: u64) -> u64 {
    super::page_table::unmap_page(x86_64::structures::paging::Page::containing_address(
        x86_64::VirtAddr::new(va))).expect("unmap uaccess test page").start_address().as_u64()
}
