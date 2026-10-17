//! Per-CPU ARM predictor policy. Unknown cores never acquire a guessed loop.
use super::types::{SpecCaps, Policy, Override, Vulnerability as V, feature as F};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum V2 { Off, Unaffected, Firmware1, Firmware3, Unavailable }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bhb { Off, Unaffected, ExceptionClear, ClearInstruction, Loop(u16), Firmware3, Unavailable }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan { pub v2: V2, pub bhb: Bhb }
impl Plan {
    pub fn v2_safe(self) -> bool { matches!(self.v2, V2::Unaffected | V2::Firmware1 | V2::Firmware3) }
    pub fn bhb_safe(self) -> bool { self.v2_safe() && !matches!(self.bhb, Bhb::Off | Bhb::Unavailable) }
}
pub fn loop_count(midr: u64) -> Option<u16> {
    if midr >> 24 & 255 != 0x41 { return None; }
    match (midr >> 4) & 0xfff {
        0xd07 | 0xd08 => Some(8), // A57/A72
        0xd0b | 0xd0c | 0xd0d | 0xd0e => Some(24), // A76/N1/A77/A76AE
        0xd40 | 0xd41 | 0xd42 | 0xd44 | 0xd47 | 0xd48 | 0xd49 | 0xd4b | 0xd4c => Some(32),
        0xd4d | 0xd81 | 0xd89 => Some(38), // A715/A720/A720AE
        0xd4e | 0xd4f => Some(132), // X3/V2
        _ => None,
    }
}
pub fn select(caps: SpecCaps, policy: Policy, conduit: bool) -> Plan {
    let force_bhb = policy.overrides[V::Bhb as usize] == Override::On;
    let bhb = if !policy.enabled(V::Bhb) { Bhb::Off }
        else if caps.not_affected & V::Bhb.bit() != 0 && !force_bhb { Bhb::Unaffected }
        else if caps.features & F::ECBHB != 0 { Bhb::ExceptionClear }
        else if caps.features & F::CLRBHB != 0 { Bhb::ClearInstruction }
        else if let Some(k) = loop_count(caps.identity) { Bhb::Loop(k) }
        else if conduit && caps.features & F::WA3 != 0 { Bhb::Firmware3 }
        else if caps.not_affected & V::Bhb.bit() != 0 { Bhb::Unaffected }
        else { Bhb::Unavailable };
    let force_v2 = policy.overrides[V::SpectreV2 as usize] == Override::On;
    let v2 = if !policy.enabled(V::SpectreV2) { V2::Off }
        else if caps.not_affected & V::SpectreV2.bit() != 0 && !force_v2 { V2::Unaffected }
        else if bhb == Bhb::Firmware3 { V2::Firmware3 } // entry WA3 supersedes switch WA1
        else if conduit && caps.features & F::WA1 != 0 { V2::Firmware1 }
        else if caps.not_affected & V::SpectreV2.bit() != 0 { V2::Unaffected }
        else { V2::Unavailable };
    Plan { v2, bhb }
}
