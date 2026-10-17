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
    // Review regressions: BHB immunity/off must not suppress a WA3-only
    // Spectre-v2 backend; return barriers are independent of root caching.
    let mut wa3_only=SpecCaps::default(); wa3_only.features=F::WA3;
    wa3_only.not_affected=V::Bhb.bit();
    for args in ["", "bhb=on", "bhb=off"] {
        let selected=select(wa3_only,Policy::parse(args).unwrap(),true);
        assert_eq!(selected.v2,V2::Firmware3);
        assert_eq!(selected.return_function(true),Some(0x80003fff));
        assert_eq!(selected.return_function(false),None);
    }
    let partial=select(wa3_only,Policy::parse("mitigations=off bhb=on").unwrap(),true);
    assert_eq!(partial.v2,V2::Off); assert_eq!(partial.bhb,Bhb::Firmware3);
    assert!(!partial.bhb_safe());
    let selected=select(fw,p,true);
    let callbacks: Vec<_> = [true,true,false,true].into_iter().filter_map(|user| selected.return_function(user)).collect();
    assert_eq!(callbacks,vec![0x80008000;3]); // adjacent same-root user resumes + one kernel return
    assert_eq!(select(fw,off,true).return_function(true),None);
    // Independent plans on heterogeneous CPUs; no global maximum/overwrite.
    assert_ne!(select(core(0xd08),p,true).bhb,select(core(0xd4e),p,true).bhb);
    println!("ARM predictor: per-core loops, firmware/conduit gates, unknown cores, immunity, overrides and partial protection passed");
}
