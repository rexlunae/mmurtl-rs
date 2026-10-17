#[path = "../../src/security/types.rs"]
#[allow(dead_code)]
mod types;
use types::*;
#[path = "../../src/security/store_bypass.rs"]
mod store_bypass;
use store_bypass::{Plan, Backend, Architecture, Mode as SsbMode};
fn main() {
    let mut caps = CommonCaps::default();
    let policy = Policy::automatic();
    let unsupported = Plan::select(caps, policy, Architecture::Amd64);
    assert_eq!(unsupported.backend, Backend::Unavailable);
    assert_eq!(unsupported.change(true), Err(u64::MAX - 3));
    assert_eq!(unsupported.change(false), Ok(false));
    assert_eq!(unsupported.query(true), Err(u64::MAX - 3));
    caps.features = feature::SSBD | feature::PSFD;
    let spec = Plan::select(caps, policy, Architecture::Amd64);
    assert_eq!(spec.backend, Backend::SpecCtrl); assert!(spec.psfd);
    caps.features = feature::VIRT_SSBD;
    assert_eq!(Plan::select(caps, policy, Architecture::Amd64).backend, Backend::VirtualSpecCtrl);
    caps.features = feature::SSBS;
    let arm = Plan::select(caps, policy, Architecture::Arm64);
    assert_eq!(arm.backend, Backend::Ssbs);
    assert_eq!(arm.mode, SsbMode::PerTask); assert!(arm.desired(true)); assert!(!arm.desired(false));
    caps.features = feature::WA2;
    assert_eq!(Plan::select(caps, policy, Architecture::Arm64).backend, Backend::Firmware);
    caps.not_affected = Vulnerability::Ssb.bit(); caps.features = 0;
    assert_eq!(Plan::select(caps, policy, Architecture::Arm64).backend, Backend::NotAffected);
    let on = Plan::select(caps, Policy::parse("ssb=on").unwrap(), Architecture::Arm64);
    assert!(on.desired(false)); assert_eq!(on.change(false), Err(u64::MAX - 4));
    let off = Plan::select(caps, Policy::parse("ssb=off").unwrap(), Architecture::Arm64);
    assert!(!off.desired(true)); assert_eq!(off.change(true), Err(u64::MAX - 4));
    assert_eq!(Policy::parse("mitigations=off ssb=per-task").unwrap().overrides[Vulnerability::Ssb as usize], Override::PerTask);
    assert!(Policy::parse("mds=per-task").is_err());
    assert_eq!(store_bypass::merge_control(3, 4, 4, 7), Some(7));
    assert_eq!(store_bypass::merge_control(7, 4, 0, 7), Some(3));
    assert_eq!(store_bypass::merge_control(3, 4 | 128, 4 | 128, 7 | 128), Some(7 | 128));
    assert_eq!(store_bypass::merge_control(3, 128, 0, 7), None);
    let flags = 0xa000_03c5 | (1 << 12);
    assert_eq!(store_bypass::ssbs_frame(flags, true, true), flags & !(1 << 12));
    assert_eq!(store_bypass::ssbs_frame(flags, false, true), flags);
    assert_eq!(store_bypass::ssbs_frame(flags, false, false), flags & !(1 << 12));
    println!("store bypass: backend gates, forced policy, unsupported requests, IBRS/STIBP preservation passed");
}
