//! Boot policy, admitted-CPU capability intersection, and truthful status.
pub mod types;
pub use types::*;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
static POLICY: spin::Once<Policy> = spin::Once::new();
static CAPS: spin::Mutex<[Option<SpecCaps>; 64]> = spin::Mutex::new([None; 64]);
static ADMISSION: spin::Mutex<()> = spin::Mutex::new(());
static ADMITTED: AtomicU64 = AtomicU64::new(1);
static SEALED: AtomicBool = AtomicBool::new(false);
static COMMON: spin::Once<CommonCaps> = spin::Once::new();
pub fn init(args: &str) {
    let policy = Policy::parse(args).unwrap_or_else(|error| {
        crate::serial::write_str("[SPEC] invalid boot policy; using auto: ");
        crate::serial::write_line(error);
        Policy::automatic()
    });
    POLICY.call_once(|| policy);
}
pub fn policy() -> Policy { POLICY.get().copied().unwrap_or(Policy::automatic()) }
pub fn record(cpu: usize, caps: SpecCaps) { CAPS.lock()[cpu] = Some(caps); }
pub fn caps(cpu: usize) -> Option<SpecCaps> { CAPS.lock()[cpu] }
pub fn admit_cpu(cpu: usize, initialize: impl FnOnce()) -> bool {
    let _guard = ADMISSION.lock();
    if SEALED.load(Ordering::Acquire) { return false; }
    assert!(caps(cpu).is_some(), "CPU admitted before capability enumeration");
    initialize();
    ADMITTED.fetch_or(1 << cpu, Ordering::Release);
    true
}
pub fn freeze() -> u64 {
    let _guard = ADMISSION.lock();
    SEALED.store(true, Ordering::Release);
    ADMITTED.load(Ordering::Acquire)
}
pub fn common() -> CommonCaps { COMMON.get().copied().unwrap_or_default() }
pub fn report() {
    let admitted = freeze();
    let common = {
        let caps = CAPS.lock();
        CommonCaps::intersection((0..64).filter(|cpu| admitted & (1 << cpu) != 0)
            .map(|cpu| caps[cpu].unwrap_or_default()))
    };
    COMMON.call_once(|| common);
    use core::fmt::Write;
    for cpu in (0..64).filter(|cpu| admitted & (1 << cpu) != 0) {
        if let Some(caps) = caps(cpu) {
            let mut line: heapless::String<256> = heapless::String::new();
            let _ = write!(line, "[SPEC] CPU{} id={:#x} features={:#x} immunity={:#x} raw={:x?}\n",
                cpu, caps.identity, caps.features, caps.not_affected, &caps.raw[..8]);
            crate::serial::write_str(&line);
        }
    }
    crate::serial::write_str("[SPEC] policy: ");
    crate::serial::write_line(match policy().mode { Mode::Off => "off", Mode::Auto => "auto", Mode::AutoNoSmt => "auto,nosmt" });
    for (family, name) in FAMILIES {
        crate::serial::write_str("[SPEC] "); crate::serial::write_str(name); crate::serial::write_str(": ");
        if common.not_affected & family.bit() != 0 {
            crate::serial::write_line("Not affected (all admitted CPUs)");
        } else if family == Vulnerability::SpectreV1 {
            crate::serial::write_line("Mitigation: always-on syscall bounds hardening");
        } else if !policy().enabled(family) {
            crate::serial::write_line("Vulnerable: disabled by policy");
        } else {
            crate::serial::write_line("Vulnerable/unknown: mitigation not yet configured");
        }
    }
    if policy().mode == Mode::AutoNoSmt {
        crate::serial::write_line("[SPEC] SMT isolation requested; topology enforcement pending (#42)");
    }
}
