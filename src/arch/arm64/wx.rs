//! Boot-only permission probes. Fixups match an armed target and exact PC.
use core::sync::atomic::{AtomicU64, Ordering};
static WRITE_TARGET: AtomicU64 = AtomicU64::new(0);
static EXEC_TARGET: AtomicU64 = AtomicU64::new(0);
#[used]
static CONSTANT: u64 = 0x12345678;
core::arch::global_asm!(r#"
.text
.global wx_write, wx_write_pc, wx_write_denied, wx_execute, wx_execute_denied, wx_text
wx_write:
    bti jc
wx_write_pc:
    strb w1, [x0]
    mov x0, #0
    ret
    dsb nsh
    isb
wx_write_denied:
    mov x0, #1
    ret
    dsb nsh
    isb
wx_execute:
    bti jc
    stp x29, x30, [sp, #-16]!
    blr x0
    dsb nsh
    isb
    mov x0, #0
    ldp x29, x30, [sp], #16
    ret
    dsb nsh
    isb
wx_execute_denied:
    mov x0, #1
    ldp x29, x30, [sp], #16
    ret
    dsb nsh
    isb
wx_text:
    bti jc
    ret
    dsb nsh
    isb
"#);
extern "C" {
    fn wx_write(ptr: u64, value: u64) -> u64;
    fn wx_execute(ptr: u64) -> u64;
    fn wx_text();
    static wx_write_pc: u8;
    static wx_write_denied: u8;
    static wx_execute_denied: u8;
    static __relro_start: u8;
    static __relro_end: u8;
}

pub fn fixup(esr: u64, pc: u64, far: u64) -> Option<u64> {
    // Only permission faults at EL1 from this boot CPU's armed probes.
    if crate::arch::cpu_index() != 0 || !(0x0d..=0x0f).contains(&(esr & 63)) { return None; }
    if esr >> 26 == 0x25 && esr & (1 << 6) != 0
        && pc == core::ptr::addr_of!(wx_write_pc) as u64
        && far != 0 && WRITE_TARGET.load(Ordering::Relaxed) == far {
        return Some(core::ptr::addr_of!(wx_write_denied) as u64);
    }
    if esr >> 26 == 0x21 && pc == far && far != 0 && EXEC_TARGET.load(Ordering::Relaxed) == far {
        return Some(core::ptr::addr_of!(wx_execute_denied) as u64);
    }
    None
}
unsafe fn deny_write(ptr: u64) {
    crate::arch::without_interrupts(|| {
        WRITE_TARGET.store(ptr, Ordering::Relaxed);
        let result = wx_write(ptr, (ptr as *const u8).read_volatile() as u64);
        WRITE_TARGET.store(0, Ordering::Relaxed);
        assert_eq!(result, 1, "kernel read-only write was allowed");
    });
}
unsafe fn deny_execute(ptr: u64) {
    crate::arch::without_interrupts(|| {
        EXEC_TARGET.store(ptr, Ordering::Relaxed);
        let result = wx_execute(ptr);
        EXEC_TARGET.store(0, Ordering::Relaxed);
        assert_eq!(result, 1, "kernel NX execution was allowed");
    });
}

pub fn self_test() {
    unsafe {
        deny_write(wx_text as *const () as u64);
        deny_write(core::ptr::addr_of!(CONSTANT) as u64);
        if core::ptr::addr_of!(__relro_start) < core::ptr::addr_of!(__relro_end) {
            deny_write(core::ptr::addr_of!(__relro_start) as u64);
        }
    }
    let Some(frame) = crate::memory::heap::with_frame_allocator(|fa| fa.allocate_zeroed_frame()).flatten() else {
        crate::serial::write_line("[MMU] NX probes skipped: out of memory"); return;
    };
    unsafe {
        let alias = super::memory::phys_to_virt(frame);
        alias.cast::<u32>().write_volatile(0xd65f03c0); // RET
        super::mmu::sync_icache(alias, 4);
        deny_execute(alias as u64);
    }
    crate::memory::heap::with_frame_allocator(|fa| fa.deallocate_frame(frame));
    // User executable pages retain a non-executable identity alias.
    let code = 0xd65f03c0u32.to_le_bytes();
    if let Ok(image) = crate::memory::user::load_program(&code) {
        let alias = unsafe { super::mmu::test_user_physical(image.space, image.entry) };
        unsafe { deny_execute(super::memory::phys_to_virt(alias) as u64); }
        crate::memory::user::release(image.space, image.slot);
    } else {
        crate::serial::write_line("[MMU] ret2dir probe skipped: out of memory"); return;
    }
    crate::serial::write_line("[MMU] W^X fault probes passed (text, rodata, RELRO, heap NX, ret2dir)");
}
