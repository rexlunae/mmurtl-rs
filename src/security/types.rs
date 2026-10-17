//! Pure policy/capability types, also exercised by host regressions.
use core::sync::atomic::{AtomicUsize, Ordering};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum Vulnerability { SpectreV1, SpectreV2, Meltdown, Ssb, Mds, Taa, Mmio, Bhi, Gds, Rfds, Srso, Retbleed, L1tf, Srbds, Its, Tsa, Bhb }
pub const FAMILIES: [(Vulnerability, &str); 17] = [
    (Vulnerability::SpectreV1,"spectre_v1"), (Vulnerability::SpectreV2,"spectre_v2"),
    (Vulnerability::Meltdown,"meltdown"), (Vulnerability::Ssb,"ssb"), (Vulnerability::Mds,"mds"),
    (Vulnerability::Taa,"taa"), (Vulnerability::Mmio,"mmio_stale_data"), (Vulnerability::Bhi,"bhi"),
    (Vulnerability::Gds,"gds"), (Vulnerability::Rfds,"rfds"), (Vulnerability::Srso,"srso"),
    (Vulnerability::Retbleed,"retbleed"), (Vulnerability::L1tf,"l1tf"), (Vulnerability::Srbds,"srbds"),
    (Vulnerability::Its,"its"), (Vulnerability::Tsa,"tsa"), (Vulnerability::Bhb,"bhb")];
impl Vulnerability { pub const fn bit(self) -> u64 { 1 << self as usize } }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode { Off, Auto, AutoNoSmt }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Override { Auto, On, Off, PerTask }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy { pub mode: Mode, pub overrides: [Override; 17] }
impl Policy {
    pub const fn automatic() -> Self { Self { mode: Mode::Auto, overrides: [Override::Auto; 17] } }
    pub fn parse(args: &str) -> Result<Self, &'static str> {
        let mut policy = Self::automatic();
        for arg in args.split_ascii_whitespace() {
            let Some((key, value)) = arg.split_once('=') else {
                if arg == "mitigations" || FAMILIES.iter().any(|(_, name)| *name == arg) {
                    return Err("missing mitigation policy value");
                }
                continue;
            };
            if key == "mitigations" {
                policy.mode = match value { "off" => Mode::Off, "auto" => Mode::Auto,
                    "auto,nosmt" => Mode::AutoNoSmt, _ => return Err("invalid mitigations mode") };
            } else if let Some((family, _)) = FAMILIES.iter().find(|(_, name)| *name == key) {
                policy.overrides[*family as usize] = match value { "auto" => Override::Auto,
                    "on" => Override::On, "off" => Override::Off,
                    "per-task" if *family == Vulnerability::Ssb => Override::PerTask, _ => return Err("invalid mitigation override") };
            }
        }
        Ok(policy)
    }
    pub fn enabled(self, family: Vulnerability) -> bool {
        match self.overrides[family as usize] { Override::On => true, Override::Off => false,
            Override::PerTask => true, Override::Auto => self.mode != Mode::Off }
    }
}
pub mod feature {
    pub const IBRS: u64 = 1 << 0; pub const STIBP: u64 = 1 << 1;
    pub const L1D_FLUSH: u64 = 1 << 2; pub const MD_CLEAR: u64 = 1 << 3;
    pub const SSBD: u64 = 1 << 4; pub const ARCH_CAPS: u64 = 1 << 5;
    pub const EIBRS: u64 = 1 << 6; pub const PSFD: u64 = 1 << 7;
    pub const IPRED_CTRL: u64 = 1 << 8; pub const RRSBA_CTRL: u64 = 1 << 9;
    pub const BHI_CTRL: u64 = 1 << 10; pub const AUTOIBRS: u64 = 1 << 11;
    pub const SBPB: u64 = 1 << 12; pub const IBPB_RET: u64 = 1 << 13;
    pub const VIRT_SSBD: u64 = 1 << 14; pub const SSBS: u64 = 1 << 15;
    pub const ECBHB: u64 = 1 << 16; pub const CLRBHB: u64 = 1 << 17;
    pub const WA1: u64 = 1 << 18; pub const WA2: u64 = 1 << 19; pub const WA3: u64 = 1 << 20;
    pub const SMEP: u64 = 1 << 21; pub const SMAP: u64 = 1 << 22; pub const UMIP: u64 = 1 << 23;
    pub const LASS: u64 = 1 << 24; pub const IBPB: u64 = 1 << 25;
    pub const DOITM: u64 = 1 << 26; pub const DIT: u64 = 1 << 27;
    pub const SSBS2: u64 = 1 << 30;
    pub const PAN: u64 = 1 << 31; pub const EPAN: u64 = 1 << 32; pub const UAO: u64 = 1 << 33;
    pub const IBT: u64 = 1 << 34; pub const BTI: u64 = 1 << 35;
    pub const RFDS_CLEAR: u64 = 1 << 28; pub const GDS_CTRL: u64 = 1 << 29;
}
#[derive(Clone, Copy, Default)]
pub struct SpecCaps {
    pub identity: u64,
    pub vendor: [u8; 12],
    pub features: u64,
    pub not_affected: u64,
    /// Architecture-specific feature words retained for future mitigations.
    pub raw: [u64; 16],
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CommonCaps { pub features: u64, pub not_affected: u64 }
impl CommonCaps {
    pub fn intersection(caps: impl IntoIterator<Item = SpecCaps>) -> Self {
        let mut caps = caps.into_iter();
        let Some(first) = caps.next() else { return Self::default(); };
        caps.fold(Self { features: first.features, not_affected: first.not_affected }, |all, cpu|
            Self { features: all.features & cpu.features, not_affected: all.not_affected & cpu.not_affected })
    }
}

/// Runtime-selected alternative. No indirect call when disabled; enabled
/// targets are trusted kernel functions selected during mitigation setup.
pub struct PatchSite { target: AtomicUsize }
impl PatchSite {
    pub const fn new() -> Self { Self { target: AtomicUsize::new(0) } }
    /// Select only an enabled, affected family's implementation whose
    /// required instruction/control features exist on every admitted CPU.
    pub fn select_checked(&self, policy: Policy, common: CommonCaps, family: Vulnerability,
        required: u64, implementation: fn()) -> bool {
        let enabled = policy.enabled(family) && common.features & required == required
            && common.not_affected & family.bit() == 0;
        self.select(enabled, implementation);
        enabled
    }
    pub fn select(&self, enabled: bool, implementation: fn()) {
        self.target.store(if enabled { implementation as *const () as usize } else { 0 }, Ordering::Release);
    }
    #[inline(always)]
    pub fn run(&self) {
        let target = self.target.load(Ordering::Acquire);
        if target != 0 { unsafe { core::mem::transmute::<usize, fn()>(target)(); } }
    }
}

/// SMCCC architecture services use a signed 32-bit return status even
/// when the firmware leaves the upper half of x0 zero or unspecified.
pub fn smccc_status(value: u64) -> i64 { value as u32 as i32 as i64 }
