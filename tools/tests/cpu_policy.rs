#[path = "../../src/security/types.rs"]
#[allow(dead_code)]
mod types;
use types::*;
#[path = "../../src/security/x86_decode.rs"]
mod x86_decode;
use std::sync::atomic::{AtomicUsize, Ordering};
static CALLS: AtomicUsize = AtomicUsize::new(0);
fn alternative() { CALLS.fetch_add(1, Ordering::Relaxed); }
fn main() {
    assert_eq!(Policy::parse("").unwrap(), Policy::automatic());
    let off = Policy::parse("console=serial mitigations=off spectre_v2=on ssb=off").unwrap();
    assert!(off.enabled(Vulnerability::SpectreV2));
    assert!(!off.enabled(Vulnerability::Ssb));
    assert!(!off.enabled(Vulnerability::Mds));
    assert_eq!(Policy::parse("mitigations=auto,nosmt").unwrap().mode, Mode::AutoNoSmt);
    assert!(Policy::parse("mitigations=typo").is_err());
    assert!(Policy::parse("ssb=typo").is_err());
    assert!(Policy::parse("mitigations=off mitigations").is_err());
    assert!(Policy::parse("mitigations=off ssb").is_err());
    let a = SpecCaps { features: feature::IBRS | feature::SSBD | feature::SMAP,
        not_affected: Vulnerability::Meltdown.bit() | Vulnerability::Mds.bit(), ..SpecCaps::default() };
    let b = SpecCaps { features: feature::SSBD, not_affected: Vulnerability::Meltdown.bit(), ..SpecCaps::default() };
    let common = CommonCaps::intersection([a, b]);
    assert_eq!(common.features, feature::SSBD);
    assert_eq!(common.not_affected, Vulnerability::Meltdown.bit());
    assert_eq!(CommonCaps::intersection([a, SpecCaps::default()]), CommonCaps::default());
    assert_eq!(CommonCaps::intersection([]), CommonCaps::default());
    assert_eq!(smccc_status(0xffff_ffff), -1);
    assert_eq!(smccc_status(u64::MAX - 1), -2);
    assert_eq!(smccc_status(0x10001), 0x10001);
    let unknown = x86_decode::decode(SpecCaps::default(), || panic!("ungated MSR read"));
    assert_eq!(unknown.features, 0);
    let mut raw = SpecCaps::default();
    raw.raw[0] = 1 << 29;
    let intel = x86_decode::decode(raw, || (1 << 0) | (1 << 5) | (1 << 1) | (1 << 62));
    assert_ne!(intel.features & feature::EIBRS, 0);
    assert_ne!(intel.not_affected & Vulnerability::Meltdown.bit(), 0);
    assert_ne!(intel.not_affected & Vulnerability::Mds.bit(), 0);
    assert_ne!(intel.not_affected & Vulnerability::Its.bit(), 0);
    raw.vendor = *b"AuthenticAMD"; raw.raw[0] = 0;
    raw.raw[3] = (1 << 8) | (1 << 29);
    let amd = x86_decode::decode(raw, || panic!("AMD without ARCH_CAPS read Intel MSR"));
    assert_ne!(amd.features & feature::AUTOIBRS, 0);
    assert_ne!(amd.not_affected & Vulnerability::Srso.bit(), 0);
    let site = PatchSite::new();
    site.run(); assert_eq!(CALLS.load(Ordering::Relaxed), 0);
    site.select(true, alternative); site.run(); assert_eq!(CALLS.load(Ordering::Relaxed), 1);
    site.select(false, alternative); site.run(); assert_eq!(CALLS.load(Ordering::Relaxed), 1);
    assert!(!site.select_checked(Policy::automatic(), common, Vulnerability::SpectreV2, feature::IBRS, alternative));
    site.run(); assert_eq!(CALLS.load(Ordering::Relaxed), 1);
    assert!(site.select_checked(Policy::automatic(), common, Vulnerability::Ssb, feature::SSBD, alternative));
    site.run(); assert_eq!(CALLS.load(Ordering::Relaxed), 2);
    assert!(!site.select_checked(Policy::automatic(), common, Vulnerability::Meltdown, 0, alternative));
    site.run(); assert_eq!(CALLS.load(Ordering::Relaxed), 2);
    println!("CPU policy: overrides, unknown CPU fail-closed intersection, heterogeneous immunity, alternatives passed");
}
