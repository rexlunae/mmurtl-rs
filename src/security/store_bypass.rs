//! Pure store-bypass policy selection; privileged backends live in arch/.
use super::{CommonCaps, Policy, Override, Vulnerability as V, feature as F};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode { Off, On, PerTask }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend { Unavailable, NotAffected, SpecCtrl, VirtualSpecCtrl, Ssbs, Firmware }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Architecture { Amd64, Arm64 }
#[derive(Clone, Copy, Debug)]
pub struct Plan { pub mode: Mode, pub backend: Backend, pub psfd: bool }
impl Plan {
    pub fn select(caps: CommonCaps, policy: Policy, arch: Architecture) -> Self {
        let mode = if !policy.enabled(V::Ssb) { Mode::Off }
            else if policy.overrides[V::Ssb as usize] == Override::On { Mode::On }
            else { Mode::PerTask };
        let backend = if caps.not_affected & V::Ssb.bit() != 0 { Backend::NotAffected }
            else { match arch {
                Architecture::Amd64 if caps.features & F::SSBD != 0 => Backend::SpecCtrl,
                Architecture::Amd64 if caps.features & F::VIRT_SSBD != 0 => Backend::VirtualSpecCtrl,
                Architecture::Arm64 if caps.features & F::SSBS != 0 => Backend::Ssbs,
                Architecture::Arm64 if caps.features & F::WA2 != 0 => Backend::Firmware,
                _ => Backend::Unavailable,
            } };
        let psfd = arch == Architecture::Amd64 && caps.features & F::PSFD != 0
            && matches!(backend, Backend::SpecCtrl | Backend::VirtualSpecCtrl);
        Self { mode, backend, psfd }
    }
    pub fn desired(self, task: bool) -> bool {
        match self.mode { Mode::Off => false, Mode::On => true, Mode::PerTask => task }
    }
    pub fn supported(self) -> bool { self.backend != Backend::Unavailable }
    /// Error codes match the syscall ABI: EPERM / EOPNOTSUPP.
    pub fn change(self, requested: bool) -> Result<bool, u64> {
        if self.mode == Mode::On && !requested || self.mode == Mode::Off && requested { return Err(u64::MAX - 4); }
        if requested && !self.supported() { return Err(u64::MAX - 3); }
        Ok(requested)
    }
    pub fn query(self, task: bool) -> Result<bool, u64> {
        let desired = self.desired(task);
        if desired && !self.supported() { Err(u64::MAX - 3) } else { Ok(desired) }
    }
}
pub fn merge_control(old: u64, clear: u64, set: u64, allowed: u64) -> Option<u64> {
    if (clear | set) & !allowed != 0 { None } else { Some((old & !clear) | set) }
}

/// Only the SSBS bit changes; clear it where the common ISA makes it RES0.
pub fn ssbs_frame(spsr: u64, disable: bool, available: bool) -> u64 {
    (spsr & !(1 << 12)) | if available && !disable { 1 << 12 } else { 0 }
}
