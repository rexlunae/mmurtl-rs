//! Per-CPU predictor vectors and cross-address-space firmware hardening.
use crate::security::{SpecCaps, Vulnerability as V};
use crate::security::arm_predictor::{self, Plan, V2, Bhb};
static PLANS: [spin::Once<Plan>; crate::scheduler::MAX_CPUS] = [const { spin::Once::new() }; crate::scheduler::MAX_CPUS];
pub fn configure(cpu: usize, caps: SpecCaps) {
    PLANS[cpu].call_once(|| arm_predictor::select(caps, crate::security::policy(), super::cpu_features::has_conduit()));
}
fn plan(cpu: usize) -> Plan { PLANS[cpu].get().copied().unwrap_or(Plan { v2: V2::Off, bhb: Bhb::Off }) }
#[inline(always)]
pub fn on_switch(cpu: usize, root: u64) {
    // Run on every activation of a different nonzero root, including a
    // reallocated root after kernel/empty-space execution. No stale root ID
    // cache can skip the barrier for a new address-space lifetime.
    if root != 0 && plan(cpu).v2 == V2::Firmware1 {
        super::cpu_features::smccc(0x8000_8000, 0);
    }
}
/// Every EL0 return, including same-root IRQ/syscall resumes. Called before
/// PAN restores user access and before any user GPR is reloaded.
#[no_mangle]
#[inline(never)]
pub unsafe extern "C" fn arm64_predictor_user_return(frame: *const super::TaskContext) {
    if let Some(function) = plan(super::cpu_index()).return_function((*frame).spsr & 15 == 0) {
        super::cpu_features::smccc(function, 0);
    }
}
extern "C" {
    static exception_vectors: u8;
    static predictor_loop8: u8;
    static predictor_loop24: u8;
    static predictor_loop32: u8;
    static predictor_loop38: u8;
    static predictor_loop132: u8;
    static predictor_clrbhb: u8;
    static predictor_wa3_hvc: u8;
    static predictor_wa3_smc: u8;
    static predictor_plain_wa1_hvc: u8;
    static predictor_plain_wa1_smc: u8;
    static predictor_clrbhb_wa1_hvc: u8;
    static predictor_clrbhb_wa1_smc: u8;
    static predictor_loop8_wa1_hvc: u8;
    static predictor_loop8_wa1_smc: u8;
    static predictor_loop24_wa1_hvc: u8;
    static predictor_loop24_wa1_smc: u8;
    static predictor_loop32_wa1_hvc: u8;
    static predictor_loop32_wa1_smc: u8;
    static predictor_loop38_wa1_hvc: u8;
    static predictor_loop38_wa1_smc: u8;
    static predictor_loop132_wa1_hvc: u8;
    static predictor_loop132_wa1_smc: u8;
}
pub fn vector() -> u64 { vector_for(super::cpu_index()) }
fn vector_for(cpu: usize) -> u64 {
    let p = plan(cpu); let hvc = super::cpu_features::hvc_conduit();
    let pointer = if p.v2 == V2::Firmware3 || p.bhb == Bhb::Firmware3 {
        if hvc { core::ptr::addr_of!(predictor_wa3_hvc) } else { core::ptr::addr_of!(predictor_wa3_smc) }
    } else if p.v2 == V2::Firmware1 {
        match p.bhb {
            Bhb::ClearInstruction => if hvc { core::ptr::addr_of!(predictor_clrbhb_wa1_hvc) } else { core::ptr::addr_of!(predictor_clrbhb_wa1_smc) },
            Bhb::Loop(8) => if hvc { core::ptr::addr_of!(predictor_loop8_wa1_hvc) } else { core::ptr::addr_of!(predictor_loop8_wa1_smc) },
            Bhb::Loop(24) => if hvc { core::ptr::addr_of!(predictor_loop24_wa1_hvc) } else { core::ptr::addr_of!(predictor_loop24_wa1_smc) },
            Bhb::Loop(32) => if hvc { core::ptr::addr_of!(predictor_loop32_wa1_hvc) } else { core::ptr::addr_of!(predictor_loop32_wa1_smc) },
            Bhb::Loop(38) => if hvc { core::ptr::addr_of!(predictor_loop38_wa1_hvc) } else { core::ptr::addr_of!(predictor_loop38_wa1_smc) },
            Bhb::Loop(132) => if hvc { core::ptr::addr_of!(predictor_loop132_wa1_hvc) } else { core::ptr::addr_of!(predictor_loop132_wa1_smc) },
            Bhb::Loop(_) => panic!("unsupported BHB loop length"),
            _ => if hvc { core::ptr::addr_of!(predictor_plain_wa1_hvc) } else { core::ptr::addr_of!(predictor_plain_wa1_smc) },
        }
    } else { match p.bhb {
        Bhb::Loop(8) => core::ptr::addr_of!(predictor_loop8),
        Bhb::Loop(24) => core::ptr::addr_of!(predictor_loop24),
        Bhb::Loop(32) => core::ptr::addr_of!(predictor_loop32),
        Bhb::Loop(38) => core::ptr::addr_of!(predictor_loop38),
        Bhb::Loop(132) => core::ptr::addr_of!(predictor_loop132),
        Bhb::ClearInstruction => core::ptr::addr_of!(predictor_clrbhb),
        Bhb::Loop(_) => panic!("unsupported BHB loop length"),
        _ => core::ptr::addr_of!(exception_vectors),
    } };
    assert_eq!(pointer as u64 & 2047, 0, "unaligned predictor vector table");
    pointer as u64
}
static INSTALLED: [core::sync::atomic::AtomicU64; crate::scheduler::MAX_CPUS] =
    [const { core::sync::atomic::AtomicU64::new(0) }; crate::scheduler::MAX_CPUS];
pub fn note_vector(actual: u64) {
    assert_eq!(actual, vector(), "predictor VBAR readback");
    INSTALLED[super::cpu_index()].store(actual, core::sync::atomic::Ordering::Release);
}
pub fn finalize() {
    let admitted = crate::security::freeze();
    let mut v2_safe = true; let mut bhb_safe = true;
    use core::fmt::Write;
    for cpu in 0..crate::scheduler::MAX_CPUS {
        if admitted & (1 << cpu) == 0 { continue; }
        assert_eq!(INSTALLED[cpu].load(core::sync::atomic::Ordering::Acquire), vector_for(cpu), "admitted CPU predictor vector missing");
        let p = plan(cpu); v2_safe &= p.v2_safe(); bhb_safe &= p.bhb_safe();
        let v2 = match p.v2 { V2::Off => "off", V2::Unaffected => "not affected", V2::Firmware1 => "firmware WA1 on entry/switch/return",
            V2::Firmware3 => "firmware WA3 on entry/return", V2::Unavailable => "VULNERABLE: firmware WA1 unavailable" };
        let mut line: heapless::String<240> = heapless::String::new();
        let _ = write!(line, "[SPEC] CPU{} predictor: Spectre v2={}; BHB=", cpu, v2);
        match p.bhb {
            Bhb::Loop(k) => { let _ = write!(line, "loop k={}", k); }
            Bhb::Off => { let _ = line.push_str("off"); }
            Bhb::Unaffected => { let _ = line.push_str("not affected"); }
            Bhb::ExceptionClear => { let _ = line.push_str("hardware ECBHB"); }
            Bhb::ClearInstruction => { let _ = line.push_str("CLRBHB + ISB"); }
            Bhb::Firmware3 => { let _ = line.push_str("firmware WA3"); }
            Bhb::Unavailable => { let _ = line.push_str("VULNERABLE: no supported backend"); }
        }
        let _ = line.push('\n'); crate::serial::write_str(&line);
    }
    if v2_safe { crate::security::installed(V::SpectreV2, "per-CPU firmware WA1/WA3 or architectural immunity"); }
    else if crate::security::policy().enabled(V::SpectreV2) { crate::security::unavailable(V::SpectreV2, "firmware workaround unavailable on at least one admitted CPU"); }
    if bhb_safe { crate::security::installed(V::Bhb, "per-CPU BHB vectors/hardware; Spectre v2 covered"); }
    else if crate::security::policy().enabled(V::Bhb) { crate::security::unavailable(V::Bhb, "BHB backend or prerequisite Spectre v2 protection unavailable"); }
}

core::arch::global_asm!(r#"
.section .text.predictor_vectors, "ax"
.macro PREDICTOR_SLOT kind, mode, count, fw, conduit
.Lpred_slot\@:
    sub sp, sp, #272
    stp x0, x1, [sp]
.if \kind >= 8
.if \mode == 1
    mov x0, #\count
.Lpred_loop\@:
    b . + 4
    subs x0, x0, #1
    b.ne .Lpred_loop\@
    dsb nsh
    isb
.elseif \mode == 2
    .inst 0xd50322df                 // CLRBHB, selected only when advertised
    isb
.endif
.if \fw != 0
    stp x2, x3, [sp, #16]
.if \fw == 1
    mov w0, #0x8000                 // WA1
.else
    mov w0, #0x3fff                 // WA3
.endif
    movk w0, #0x8000, lsl #16       // architecture fast call
    mov x1, xzr
    mov x2, xzr
    mov x3, xzr
.if \conduit == 1
    hvc #0
.else
    smc #0
.endif
    ldp x2, x3, [sp, #16]          // common entry saves these originals
.endif
.endif
    mov x0, #\kind
    b exception_common
    .org .Lpred_slot\@ + 128, 0     // rejects oversized vector entries
.endm
.macro PREDICTOR_TABLE name, mode, count, fw, conduit
    .balign 2048
    .global \name
\name:
    PREDICTOR_SLOT 0, \mode, \count, \fw, \conduit
    PREDICTOR_SLOT 1, \mode, \count, \fw, \conduit
    PREDICTOR_SLOT 2, \mode, \count, \fw, \conduit
    PREDICTOR_SLOT 3, \mode, \count, \fw, \conduit
    PREDICTOR_SLOT 4, \mode, \count, \fw, \conduit
    PREDICTOR_SLOT 5, \mode, \count, \fw, \conduit
    PREDICTOR_SLOT 6, \mode, \count, \fw, \conduit
    PREDICTOR_SLOT 7, \mode, \count, \fw, \conduit
    PREDICTOR_SLOT 8, \mode, \count, \fw, \conduit
    PREDICTOR_SLOT 9, \mode, \count, \fw, \conduit
    PREDICTOR_SLOT 10, \mode, \count, \fw, \conduit
    PREDICTOR_SLOT 11, \mode, \count, \fw, \conduit
    PREDICTOR_SLOT 12, \mode, \count, \fw, \conduit
    PREDICTOR_SLOT 13, \mode, \count, \fw, \conduit
    PREDICTOR_SLOT 14, \mode, \count, \fw, \conduit
    PREDICTOR_SLOT 15, \mode, \count, \fw, \conduit
    .endm
    PREDICTOR_TABLE predictor_loop8, 1, 8, 0, 0
    PREDICTOR_TABLE predictor_loop24, 1, 24, 0, 0
    PREDICTOR_TABLE predictor_loop32, 1, 32, 0, 0
    PREDICTOR_TABLE predictor_loop38, 1, 38, 0, 0
    PREDICTOR_TABLE predictor_loop132, 1, 132, 0, 0
    PREDICTOR_TABLE predictor_clrbhb, 2, 0, 0, 0
    PREDICTOR_TABLE predictor_wa3_hvc, 0, 0, 3, 1
    PREDICTOR_TABLE predictor_wa3_smc, 0, 0, 3, 2
    PREDICTOR_TABLE predictor_plain_wa1_hvc, 0, 0, 1, 1
    PREDICTOR_TABLE predictor_plain_wa1_smc, 0, 0, 1, 2
    PREDICTOR_TABLE predictor_clrbhb_wa1_hvc, 2, 0, 1, 1
    PREDICTOR_TABLE predictor_clrbhb_wa1_smc, 2, 0, 1, 2
    PREDICTOR_TABLE predictor_loop8_wa1_hvc, 1, 8, 1, 1
    PREDICTOR_TABLE predictor_loop8_wa1_smc, 1, 8, 1, 2
    PREDICTOR_TABLE predictor_loop24_wa1_hvc, 1, 24, 1, 1
    PREDICTOR_TABLE predictor_loop24_wa1_smc, 1, 24, 1, 2
    PREDICTOR_TABLE predictor_loop32_wa1_hvc, 1, 32, 1, 1
    PREDICTOR_TABLE predictor_loop32_wa1_smc, 1, 32, 1, 2
    PREDICTOR_TABLE predictor_loop38_wa1_hvc, 1, 38, 1, 1
    PREDICTOR_TABLE predictor_loop38_wa1_smc, 1, 38, 1, 2
    PREDICTOR_TABLE predictor_loop132_wa1_hvc, 1, 132, 1, 1
    PREDICTOR_TABLE predictor_loop132_wa1_smc, 1, 132, 1, 2
    .global predictor_vectors_end
predictor_vectors_end:
"#);
