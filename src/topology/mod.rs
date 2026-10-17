//! CPU topology and one-scheduling-thread-per-core admission.
pub mod policy;
use core::sync::atomic::{AtomicBool, Ordering};
use crate::security::{Policy, Mode};
use policy::{Admission, Reason, Source};
static STATE: spin::Mutex<Admission<{crate::scheduler::MAX_CPUS}>> = spin::Mutex::new(Admission::new(true));
static CONFIGURED: AtomicBool = AtomicBool::new(false);
pub fn init(args: &str, policy: Policy) {
    assert!(!CONFIGURED.swap(true, Ordering::AcqRel), "topology configured twice");
    let disable = policy::disable_smt(args, policy.mode == Mode::Off, policy.mode == Mode::AutoNoSmt).unwrap_or_else(|_| {
        crate::serial::write_line("[SMP] invalid nosmt option; isolating siblings"); true
    });
    let mut state = STATE.lock(); *state = Admission::new(disable);
}
/// Probe the BSP after its local interrupt controller is enabled: older AMD
/// extended APIC IDs are reserved while IA32_APIC_BASE.APICEn is clear.
pub fn bsp_ready() {
    let mut state = STATE.lock();
    assert_eq!(state.records[0].reason, Reason::Empty, "BSP topology initialized twice");
    state.bsp(crate::arch::topology::probe());
}
/// Called inside the security admission seal. On x86 the prepared CPU has
/// guarded NMI/MC gates before parking; on ARM it parks with DAIF masked.
pub fn admit(cpu: usize) -> bool { STATE.lock().admit(cpu, crate::arch::topology::probe()) }
pub fn isolating() -> bool { CONFIGURED.load(Ordering::Acquire) && STATE.lock().disable_smt }
pub fn core_id(cpu: usize) -> Option<u64> {
    let state = STATE.lock();
    state.records.get(cpu).filter(|r| r.reason == Reason::Online && r.core.known()).map(|r| r.core.base)
}
pub fn report() {
    let admitted = crate::security::freeze();
    let state = STATE.lock();
    let mut logical = 0; let mut known_cores = 0; let mut unknown = 0;
    let mut siblings = 0; let mut unknown_offline = 0; let mut max_threads = 0;
    use core::fmt::Write;
    for (cpu, r) in state.records.iter().enumerate() {
        if r.reason == Reason::Empty { continue; }
        logical += 1;
        assert_eq!(r.reason == Reason::Online, admitted & (1 << cpu) != 0, "topology/scheduler admission mismatch");
        if r.reason == Reason::Sibling { siblings += 1; }
        if r.reason == Reason::Unknown { unknown_offline += 1; }
        if !r.core.known() { unknown += 1; }
        else if !state.records[..cpu].iter().any(|old| old.reason != Reason::Empty && old.core.known() && old.core.overlaps(r.core)) { known_cores += 1; }
        if r.core.known() {
            max_threads = max_threads.max(state.records.iter().filter(|other| other.reason != Reason::Empty && other.core.known() && other.core.overlaps(r.core)).count());
        }
        let source = match r.core.source { Source::Intel1f => "CPUID.1F", Source::IntelB => "CPUID.B",
            Source::Amd1e => "AMD.1E", Source::Legacy => "legacy CPUID", Source::ArmAffinity => "MPIDR affinity", Source::Unknown => "unknown" };
        let mut line: heapless::String<160> = heapless::String::new();
        let _ = write!(line, "[SMP] topology CPU{}: logical={} core={} thread={} package={:?} source={} {}\n", cpu,
            r.core.logical, r.core.base, r.core.logical & r.core.mask, r.core.package, source,
            if r.reason == Reason::Online { "online" } else { "offline" });
        crate::serial::write_str(&line);
    }
    let mut line: heapless::String<180> = heapless::String::new();
    let _ = write!(line, "[SMP] topology: {} known core groups, {} logical CPUs, up to {} discovered threads/core, {} unknown\n", known_cores, logical, max_threads, unknown);
    crate::serial::write_str(&line);
    line.clear();
    if state.disable_smt { let _ = write!(line, "[SMP] nosmt: {} sibling threads left offline; {} unknown CPUs parked\n", siblings, unknown_offline); }
    else { let _ = line.push_str("[SMP] SMT allowed by policy: simultaneous cross-thread isolation disabled\n"); }
    crate::serial::write_str(&line);
}
