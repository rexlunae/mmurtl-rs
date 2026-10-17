#![allow(dead_code)]
#[path="../../src/security/types.rs"] mod types;
pub use types::{SpecCaps,Vulnerability,feature};
#[path="../../src/security/x86_decode.rs"] mod decode;
#[path="../../src/security/cfi_policy.rs"] mod policy;
fn main(){
    let mut caps=SpecCaps::default();caps.vendor=*b"GenuineIntel";caps.raw[0]=1<<20;
    assert_eq!(decode::decode(caps,||panic!("ungated MSR read")).features,feature::IBT);
    caps.raw[0]=0;assert_eq!(decode::decode(caps,||panic!("ungated MSR read")).features,0);
    for field in 0..16 {assert_eq!(policy::arm_bti(field),field==1);}
    assert!(policy::arm_bti((u64::MAX&!15)|1));
    let supported=SpecCaps {features:feature::IBT|feature::BTI,..SpecCaps::default()};
    let missing=SpecCaps::default();
    assert_eq!(types::CommonCaps::intersection([supported,supported].into_iter()).features,supported.features);
    assert_eq!(types::CommonCaps::intersection([supported,missing].into_iter()).features,0);
    assert_eq!(types::CommonCaps::intersection([].into_iter()).features,0);
    assert_eq!(policy::SUPERVISOR_ENDBR&((1<<0)|(1<<3)|(1<<4)|(1<<10)|(1<<11)),0);
    assert_eq!(policy::CR4_CET,1<<23);assert_eq!(policy::SCTLR_BT1,1<<36);assert_eq!(policy::PTE_GP,1<<50);
    println!("CFI policy: CPUID/PFR1 fields, fail-closed common capabilities and strict control ownership passed");
}
