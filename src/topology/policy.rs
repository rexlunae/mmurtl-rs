//! Allocation-free SMT admission and topology interpretation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source { Intel1f, IntelB, Amd1e, Legacy, ArmAffinity, Unknown }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Core { pub logical: u64, pub base: u64, pub mask: u64, pub package: Option<u64>, pub threads: u32, pub source: Source }
impl Core {
    pub const fn unknown(logical: u64) -> Self { Self { logical, base: logical, mask: 0, package: None, threads: 0, source: Source::Unknown } }
    pub fn x86(id: u32, smt_shift: u32, package_shift: u32, threads: u32, source: Source) -> Option<Self> {
        if smt_shift > 31 || package_shift > 31 || package_shift < smt_shift
            || threads == 0 || u64::from(threads) > (1u64 << smt_shift) { return None; }
        let mask = (1u64 << smt_shift) - 1;
        Some(Self { logical: id as u64, base: id as u64 & !mask, mask,
            package: Some((id as u64) >> package_shift), threads, source })
    }
    pub fn arm(mpidr: u64) -> Self {
        let id = mpidr & 0xff00ffffff; // Aff3, Aff2, Aff1, Aff0 only
        let mask = if mpidr & (1 << 24) != 0 { 255 } else { 0 };
        Self { logical: id, base: id & !mask, mask, package: None,
            threads: if mask == 0 { 1 } else { 256 }, source: Source::ArmAffinity }
    }
    pub fn known(self) -> bool { self.source != Source::Unknown }
    /// Different local SMT widths may describe the same core (hybrid CPUs).
    /// Conservatively overlap their APIC/affinity ranges instead of comparing
    /// locally shifted core numbers, which can alias across widths/packages.
    pub fn overlaps(self, other: Self) -> bool { (self.base ^ other.base) & !(self.mask | other.mask) == 0 }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason { Empty, Online, Sibling, Unknown }
#[derive(Clone, Copy, Debug)]
pub struct Record { pub core: Core, pub reason: Reason }
impl Record { const EMPTY: Self = Self { core: Core::unknown(0), reason: Reason::Empty }; }
pub struct Admission<const N: usize> { pub disable_smt: bool, pub records: [Record; N] }
impl<const N: usize> Admission<N> {
    pub const fn new(disable_smt: bool) -> Self { Self { disable_smt, records: [Record::EMPTY; N] } }
    pub fn bsp(&mut self, core: Core) { self.records[0] = Record { core, reason: Reason::Online }; }
    pub fn admit(&mut self, cpu: usize, core: Core) -> bool {
        if cpu == 0 || cpu >= N || self.records[cpu].reason != Reason::Empty { return false; }
        let reason = if !self.disable_smt { Reason::Online }
            else if !core.known() || !self.records[0].core.known() { Reason::Unknown }
            else if self.records.iter().any(|r| r.reason == Reason::Online && core.overlaps(r.core)) { Reason::Sibling }
            else { Reason::Online };
        self.records[cpu] = Record { core, reason };
        reason == Reason::Online
    }
}
pub fn disable_smt(args: &str, mitigations_off: bool, forced_nosmt: bool) -> Result<bool, ()> {
    let mut disable = !mitigations_off;
    let mut forced = forced_nosmt;
    for arg in args.split_ascii_whitespace() {
        if arg == "nosmt" { disable = true; }
        else if let Some(value) = arg.strip_prefix("nosmt=") {
            match value { "on" => disable = true, "off" => disable = false,
                "auto" => disable = !mitigations_off, "force" => { forced = true; disable = true; },
                _ => return Err(()) }
        }
    }
    Ok(forced || disable)
}
