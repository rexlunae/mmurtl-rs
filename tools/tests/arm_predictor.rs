#![allow(dead_code)]
#[path="../../src/security/types.rs"] mod types;
#[path="../../src/security/arm_predictor.rs"] mod predictor;
use types::{SpecCaps, Policy, Vulnerability as V, feature as F};
use predictor::{select, Bhb, V2};
fn core(part:u64)->SpecCaps { SpecCaps { identity: (0x41<<24)|(part<<4), ..SpecCaps::default() } }
fn main() {
    let p=Policy::automatic();
    let a72=core(0xd08);
    assert_eq!(select(a72,p,true).bhb,Bhb::Loop(8));
    assert_eq!(select(a72,p,true).v2,V2::Unavailable);
    assert!(!select(a72,p,true).bhb_safe());
    let mut fw=a72; fw.features=F::WA1;
    assert_eq!(select(fw,p,true).v2,V2::Firmware1); assert!(select(fw,p,true).bhb_safe());
    assert_eq!(select(fw,p,false).v2,V2::Unavailable);
    let mut unknown=SpecCaps::default();
    assert_eq!(select(unknown,p,true).bhb,Bhb::Unavailable);
    unknown.features=F::WA3;
    assert_eq!(select(unknown,p,true).bhb,Bhb::Firmware3);
    assert_eq!(select(unknown,p,true).v2,V2::Firmware3);
    assert!(select(unknown,p,true).bhb_safe());
    unknown.features=F::CLRBHB|F::WA1;
    assert_eq!(select(unknown,p,true).bhb,Bhb::ClearInstruction);
    unknown.features=F::ECBHB|F::CLRBHB|F::WA1;
    assert_eq!(select(unknown,p,true).bhb,Bhb::ExceptionClear);
    unknown.not_affected=V::SpectreV2.bit()|V::Bhb.bit();
    assert_eq!(select(unknown,p,true).bhb,Bhb::Unaffected);
    let off=Policy::parse("mitigations=off").unwrap();
    assert_eq!(select(fw,off,true).v2,V2::Off); assert_eq!(select(fw,off,true).bhb,Bhb::Off);
    let forced=Policy::parse("mitigations=off bhb=on spectre_v2=on").unwrap();
    assert_eq!(select(unknown,forced,true).bhb,Bhb::ExceptionClear);
    assert_eq!(select(unknown,forced,true).v2,V2::Firmware1);
    unknown.features=0;
    assert_eq!(select(unknown,forced,true).v2,V2::Unaffected);
    assert_eq!(select(unknown,forced,true).bhb,Bhb::Unaffected);
    for (part,k) in [(0xd07,8),(0xd0b,24),(0xd41,32),(0xd4d,38),(0xd4e,132),(0xd81,38)] {
        assert_eq!(predictor::loop_count(core(part).identity),Some(k));
    }
    for part in 0..4096 {
        let k=predictor::loop_count(core(part).identity);
        assert!(k.is_none() || matches!(k,Some(8|24|32|38|132)));
        assert_eq!(predictor::loop_count((0x42<<24)|(part<<4)),None);
    }
    // Independent plans on heterogeneous CPUs; no global maximum/overwrite.
    assert_ne!(select(core(0xd08),p,true).bhb,select(core(0xd4e),p,true).bhb);
    println!("ARM predictor: per-core loops, firmware/conduit gates, unknown cores, immunity, overrides and partial protection passed");
}
