#![allow(dead_code)]
#[path="../../src/security/types.rs"] mod types;
pub use types::*;
#[path="../../src/security/x86_predictor.rs"] mod predictor;
use predictor::{Plan,Mode as PredictorMode};
fn main() {
    let auto=Policy::automatic();
    let caps=CommonCaps::default();
    let p=Plan::select(caps,auto,true,true);assert_eq!(p.mode,PredictorMode::Retpolines);assert_eq!(p.set,0);assert!(p.bhb_loop&&p.rsb&&!p.ibpb);
    let f=feature::IBRS|feature::EIBRS|feature::IBPB|feature::STIBP|feature::BHI_CTRL|feature::RRSBA_CTRL|feature::IPRED_CTRL;
    let p=Plan::select(CommonCaps {features:f,not_affected:0},auto,true,false);
    assert_eq!(p.mode,PredictorMode::EnhancedIbrs);assert_eq!(p.set,1|2|1<<6|1<<10|1<<4);assert!(p.ibpb&&p.ibhf&&p.sibling_safe&&!p.bhb_loop);
    let p=Plan::select(CommonCaps {features:feature::AUTOIBRS,not_affected:0},auto,false,true);assert_eq!(p.mode,PredictorMode::AutoIbrs);assert_eq!(p.set,0);
    let p=Plan::select(CommonCaps {features:feature::IBRS,not_affected:0},auto,true,true);assert_eq!(p.mode,PredictorMode::LegacyIbrs);assert_eq!(p.set,1);
    let p=Plan::select(CommonCaps {features:feature::EIBRS,not_affected:0},auto,true,true);assert_eq!(p.mode,PredictorMode::Retpolines);assert_eq!(p.set,0);
    let off=Policy::parse("mitigations=off").unwrap();
    let p=Plan::select(CommonCaps {features:f,not_affected:0},off,true,false);
    assert_eq!(p.mode,PredictorMode::Off);assert_eq!(p.set,0);assert_ne!(p.clear,0);assert!(!p.rsb&&!p.ibpb&&!p.bhb_loop&&!p.ibhf);
    let p=Plan::select(caps,auto,false,false);assert!(!p.sibling_safe);
    let p=Plan::select(CommonCaps {features:0,not_affected:Vulnerability::Bhi.bit()},auto,true,true);assert!(!p.bhb_loop&&!p.ibhf);
    let mixed=CommonCaps::intersection([SpecCaps {features:f,..SpecCaps::default()},SpecCaps::default()]);
    assert_eq!(Plan::select(mixed,auto,true,true).set,0);
    println!("x86 predictor: gated control ownership, eIBRS/AutoIBRS/legacy fallback, BHI, SMT and off/heterogeneous policies passed");
}
