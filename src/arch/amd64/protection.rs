//! Enable only access-protection features shared by every online CPU.
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use crate::security::x86_access::{SMEP, SMAP, UMIP, LASS};
static CAPS: [AtomicU64; 64] = [const { AtomicU64::new(0) }; 64];
static INITIALIZED: AtomicU64 = AtomicU64::new(0);
static APPLIED: [AtomicBool; 64] = [const { AtomicBool::new(false) }; 64];
static READY: AtomicBool = AtomicBool::new(false);
static COMMON: AtomicU64 = AtomicU64::new(0);
/// Assembly entries read this byte without clobbering any incoming register.
/// Published only after every online CPU's support has been intersected.
#[no_mangle]
pub static SMAP_ENTRY_ENABLED: AtomicU8 = AtomicU8::new(0);

pub fn enumerate(cpu: usize) {
    super::cpu_features::enumerate(cpu);
    let features = crate::security::caps(cpu).expect("missing CPU capabilities").features;
    use crate::security::feature as F;
    let mut caps = 0;
    for (feature, flag) in [(F::SMEP, SMEP), (F::SMAP, SMAP), (F::UMIP, UMIP), (F::LASS, LASS)] {
        if features & feature != 0 { caps |= flag; }
    }
    unsafe {
        let mut cr4: u64;
        core::arch::asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack));
        cr4 &= !(SMEP | SMAP | UMIP | LASS);
        core::arch::asm!("mov cr4, {}", in(reg) cr4, options(nostack));
        core::arch::asm!("pushfq", "and qword ptr [rsp], -262145", "popfq"); // clear inherited AC
    }
    CAPS[cpu].store(caps, Ordering::Release);
    INITIALIZED.fetch_or(1 << cpu, Ordering::Release);
}

pub fn finalize() {
    let admitted = crate::security::freeze();
    let mut common = crate::security::x86_access::common((0..64)
        .filter(|cpu| admitted & (1 << cpu) != 0)
        .map(|cpu| CAPS[cpu].load(Ordering::Acquire)));
    // Parked SMT siblings retain guarded NMI/MC gates. Shared entry CLAC
    // must also be legal on them, even though they never run user tasks.
    let initialized = INITIALIZED.load(Ordering::Acquire);
    if (0..64).filter(|cpu| initialized & (1 << cpu) != 0)
        .any(|cpu| CAPS[cpu].load(Ordering::Acquire) & SMAP == 0) {
        common = crate::security::x86_access::common([common & !SMAP].into_iter());
    }
    COMMON.store(common, Ordering::Release);
    SMAP_ENTRY_ENABLED.store(u8::from(common & SMAP != 0), Ordering::Release);
    READY.store(true, Ordering::Release);
    apply_current(0);
    assert!(!admit_ap(63, || panic!("late AP admitted after finalization")));
    crate::serial::write_line("[TEST] finalized CPU admission rejects late starters");
    crate::serial::write_str("[CPU] common access protections:");
    for (mask, name) in [(SMEP, " SMEP"), (SMAP, " SMAP"), (UMIP, " UMIP"), (LASS, " LASS")] {
        if common & mask != 0 { crate::serial::write_str(name); }
    }
    crate::serial::write_line(" (enforced before each CPU runs tasks)");
    crate::serial::write_line(if common & LASS != 0 { "[CPU] LASS enabled (higher-half kernel; STAC/CLAC user access)" }
        else { "[CPU] LASS: unavailable on common CPU feature set (requires SMAP)" });
}

/// Serialize admission with finalization. A late AP is rejected before
/// loading the shared IDT, whose CLAC policy might be unsupported there.
/// CPU indices need not be contiguous; only admitted CPUs are intersected.
pub fn admit_ap(cpu: usize, initialize: impl FnOnce()) -> bool {
    crate::security::admit_cpu(cpu, initialize)
}

/// Called with interrupts disabled before choosing a task's page tables.
/// APs may have parked before finalize; they apply the common policy on
/// their first scheduler entry, before any task can enter ring 3.
pub fn apply_current(cpu: usize) {
    // Until finalize, don't mark this CPU configured: an early idle tick
    // must not prevent the final policy from being applied later.
    if !READY.load(Ordering::Acquire) || APPLIED[cpu].load(Ordering::Relaxed) { return; }
    let common = COMMON.load(Ordering::Acquire);
    if common & LASS != 0 {
        let stack: u64;
        unsafe { core::arch::asm!("mov {}, rsp", out(reg) stack, options(nomem, nostack)); }
        assert!(stack >> 63 == 1 && apply_current as *const () as u64 >> 63 == 1
            && super::page_table::physical_memory_offset() >> 63 == 1, "LASS requires higher-half kernel mappings");
    }
    unsafe {
        let (mut cr0, mut cr4): (u64, u64);
        core::arch::asm!("mov {}, cr0", out(reg) cr0, options(nomem, nostack));
        core::arch::asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack));
        cr0 |= 1 << 16; // WP: supervisor writes obey read-only mappings
        cr4 |= common;
        core::arch::asm!("mov cr0, {}", in(reg) cr0, options(nostack));
        core::arch::asm!("mov cr4, {}", in(reg) cr4, options(nostack));
        clear_access();
        core::arch::asm!("mov {}, cr0", out(reg) cr0, options(nomem, nostack));
        core::arch::asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack));
        assert!(cr0 & (1 << 16) != 0 && cr4 & common == common);
    }
    APPLIED[cpu].store(true, Ordering::Release);
}

pub fn smap_enabled() -> bool { SMAP_ENTRY_ENABLED.load(Ordering::Relaxed) != 0 }
#[inline(always)]
pub fn clear_access() {
    if smap_enabled() { unsafe { core::arch::asm!("clac", options(nostack)); } }
}
pub fn lass_enabled() -> bool { COMMON.load(Ordering::Acquire) & LASS != 0 }
pub fn umip_enabled() -> bool { COMMON.load(Ordering::Acquire) & UMIP != 0 }

// Deliberately faulting boot probes. The private gate is DPL0, and the
// dynamic instruction-fetch fixup is armed only in the private test space
// before creating any runnable user tasks.
static EXEC_TEST_ADDR: AtomicU64 = AtomicU64::new(0);
core::arch::global_asm!(r#"
    .text
    .globl protection_smap_probe
protection_smap_probe:
    stac
    int 0x83
    clac
    ret
    int3
    .globl protection_test_gate
protection_test_gate:
    cmp byte ptr [rip + SMAP_ENTRY_ENABLED], 0
    je 2f
    clac
2:
.Lsmap_fault:
    movzx eax, byte ptr [rdi]
    xor eax, eax
    iretq
    int3
.Lsmap_denied:
    mov eax, 1
    iretq
    int3
    .globl protection_smep_probe
protection_smep_probe:
    call rdi
    xor eax, eax
    ret
    int3
    .globl protection_smep_denied
protection_smep_denied:
    add rsp, 8
    mov eax, 1
    ret
    int3
    .pushsection .rodata.ex_table, "a"
    .balign 8
    .globl protection_test_ex_table
protection_test_ex_table:
    .quad .Lsmap_fault - .
    .quad .Lsmap_denied - .
    .popsection
"#);
extern "C" {
    fn protection_smap_probe(ptr: u64) -> u64;
    fn protection_smep_probe(ptr: u64) -> u64;
    fn protection_test_gate();
    fn protection_smep_denied();
    static protection_test_ex_table: [i64; 2];
}
pub fn test_gate_address() -> u64 { protection_test_gate as *const () as u64 }
pub fn test_fixup(pc: u64, address: u64, error: u64) -> Option<u64> {
    unsafe {
        let entry = core::ptr::addr_of!(protection_test_ex_table).cast::<i64>();
        if pc == (entry as u64).wrapping_add_signed(entry.read()) {
            return Some((entry.add(1) as u64).wrapping_add_signed(entry.add(1).read()));
        }
    }
    let expected = EXEC_TEST_ADDR.load(Ordering::Acquire);
    if expected != 0 && pc == expected && address == expected
        && error & 0x15 == 0x11 && super::cpu_index() == 0 {
        return Some(protection_smep_denied as *const () as u64);
    }
    None
}

/// LASS faults before paging with #GP(0), so CR2 is not meaningful.
/// Recover only exact private probe PCs, with the fetch target armed on BSP.
pub fn test_gp_fixup(pc: u64, error: u64) -> Option<u64> {
    if error != 0 || super::cpu_index() != 0 { return None; }
    unsafe {
        let entry = core::ptr::addr_of!(protection_test_ex_table).cast::<i64>();
        if pc == (entry as u64).wrapping_add_signed(entry.read()) {
            return Some((entry.add(1) as u64).wrapping_add_signed(entry.add(1).read()));
        }
    }
    let expected = EXEC_TEST_ADDR.load(Ordering::Acquire);
    if expected != 0 && pc == expected && (crate::memory::user::USER_BASE..crate::memory::user::USER_END).contains(&pc) {
        return Some(protection_smep_denied as *const () as u64);
    }
    None
}

pub fn self_test() {
    let common = COMMON.load(Ordering::Acquire);
    let img = match crate::memory::user::load_program(&[0xc3]) { // RET
        Ok(img) => img,
        Err(_) => { crate::serial::write_line("[TEST] access protections skipped: no test memory"); return; }
    };
    let mut pass = true;
    // Canonicality also raises #GP(0), allowing the exact copy/gate
    // recovery path to be tested even on CPUs without LASS.
    let invalid = 0x0000_8000_0000_0000u64;
    let mut byte = 0u8;
    pass &= unsafe { super::uaccess::copy_from_user(&mut byte, invalid, 1) } == 1;
    pass &= unsafe { super::uaccess::copy_to_user(invalid, &byte, 1) } == 1;
    if common & SMAP != 0 { pass &= unsafe { protection_smap_probe(invalid) } == 1; }
    assert!(pass, "#GP user-copy/private-gate fixups failed");
    crate::serial::write_line("[TEST] #GP fixups passed (noncanonical read/write, private gate where SMAP present)");
    let text = self_test as *const () as u64;
    let pa = super::page_table::translate_virtual(x86_64::VirtAddr::new(text)).expect("test text unmapped");
    let alias = super::page_table::phys_to_virt(pa).as_u64();
    let original = unsafe { (alias as *const u8).read_volatile() };
    pass &= unsafe { super::uaccess::copy_to_user(alias, [original].as_ptr(), 1) } == 1;
    pass &= unsafe { (alias as *const u8).read_volatile() } == original;
    pass &= unsafe { super::uaccess::copy_to_user(text, [original].as_ptr(), 1) } == 1;
    let user_pa = super::without_interrupts(|| {
        super::switch_address_space(0, img.space);
        let pa = super::page_table::translate_virtual(x86_64::VirtAddr::new(img.entry)).expect("user code");
        super::switch_address_space(0, 0);
        pa
    });
    let user_alias = super::page_table::phys_to_virt(user_pa).as_u64();
    super::without_interrupts(|| {
        EXEC_TEST_ADDR.store(user_alias, Ordering::Release);
        pass &= unsafe { protection_smep_probe(user_alias) } == 1;
        EXEC_TEST_ADDR.store(0, Ordering::Release);
    });
    if let Some(frame) = crate::memory::heap::with_frame_allocator(|fa| fa.allocate_zeroed_frame()).flatten() {
        let heap_alias = super::page_table::phys_to_virt(x86_64::PhysAddr::new(frame)).as_u64();
        unsafe { (heap_alias as *mut u8).write_volatile(0xc3); }
        super::without_interrupts(|| {
            EXEC_TEST_ADDR.store(heap_alias, Ordering::Release);
            pass &= unsafe { protection_smep_probe(heap_alias) } == 1;
            EXEC_TEST_ADDR.store(0, Ordering::Release);
        });
        crate::memory::heap::with_frame_allocator(|fa| fa.deallocate_frame(frame));
    } else { crate::serial::write_line("[MMU] Heap NX probe skipped: out of memory"); }
    super::without_interrupts(|| {
        super::switch_address_space(0, img.space);
        if common & SMAP != 0 {
            // The gate must clear AC that we deliberately set before entry.
            pass &= unsafe { protection_smap_probe(img.entry) } == 1;
        }
        if common & SMEP != 0 {
            EXEC_TEST_ADDR.store(img.entry, Ordering::Release);
            pass &= unsafe { protection_smep_probe(img.entry) } == 1;
            EXEC_TEST_ADDR.store(0, Ordering::Release);
        }
        super::switch_address_space(0, 0);
    });
    crate::memory::user::release(img.space, img.slot);
    assert!(pass, "kernel W^X / access protection fault probes failed");
    crate::serial::write_line("[MMU] W^X fault probes passed (text, physmap RO, heap NX, ret2dir)");
    crate::serial::write_line("[TEST] access protections: inherited AC cleared, unguarded read/execute and physmap writes denied");
}

/// Syscall entry must close user-controlled AC before any Rust handler.
pub fn assert_syscall_entry() {
    if smap_enabled() {
        let flags: u64;
        unsafe { core::arch::asm!("pushfq", "pop {}", out(reg) flags); }
        assert!(flags & (1 << 18) == 0, "user AC leaked through syscall entry");
    }
}
