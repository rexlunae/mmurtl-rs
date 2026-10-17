#![allow(dead_code)]
#[path="../../src/topology/policy.rs"] mod topology;
use topology::{Core, Source, Admission, Reason, disable_smt};
fn core(id:u32,shift:u32)->Core { Core::x86(id,shift,6,1<<shift,Source::IntelB).unwrap() }
fn main() {
    assert_eq!(disable_smt("",false,false),Ok(true));
    assert_eq!(disable_smt("",true,false),Ok(false));
    assert_eq!(disable_smt("nosmt=off",false,false),Ok(false));
    assert_eq!(disable_smt("nosmt",true,false),Ok(true));
    assert_eq!(disable_smt("nosmt=force nosmt=off",true,false),Ok(true));
    assert_eq!(disable_smt("nosmt=off",false,true),Ok(true));
    assert_eq!(disable_smt("nosmt=bogus",false,false),Err(()));
    assert!(Core::x86(0,32,32,2,Source::IntelB).is_none());
    assert!(Core::x86(0,2,1,4,Source::IntelB).is_none());
    assert!(Core::x86(0,0,0,0,Source::IntelB).is_none());
    assert!(Core::x86(0,1,2,3,Source::IntelB).is_none());
    let mut a=Admission::<8>::new(true); a.bsp(core(0,1));
    for id in 1..8 { assert_eq!(a.admit(id as usize,core(id,1)),id%2==0); }
    assert_eq!(a.records.iter().filter(|r|r.reason==Reason::Online).count(),4);
    assert_eq!(a.records[7].reason,Reason::Sibling);
    assert!(!a.admit(8,core(8,1)) && !a.admit(0,core(0,1)) && !a.admit(2,core(2,1)));
    // A BSP need not be hardware thread zero; enumeration order is arbitrary.
    let mut a=Admission::<8>::new(true); a.bsp(core(1,1));
    assert!(!a.admit(1,core(0,1))); assert!(a.admit(2,core(4,1))); assert!(a.admit(3,core(2,1)));
    assert!(!a.admit(4,core(5,1)) && !a.admit(5,core(3,1)));
    // Full APIC keys distinguish packages; local SMT widths may differ.
    let p0=Core::x86(0,1,2,2,Source::Intel1f).unwrap();
    let p1=Core::x86(4,1,2,2,Source::Intel1f).unwrap();
    assert_eq!(p0.package,Some(0)); assert_eq!(p1.package,Some(1)); assert!(!p0.overlaps(p1));
    let mut a=Admission::<4>::new(true); a.bsp(core(0,1));
    assert!(!a.admit(1,core(1,0))); assert!(a.admit(2,core(2,0))); assert!(!a.admit(3,core(3,1)));
    let mut a=Admission::<4>::new(true); a.bsp(Core::unknown(0));
    assert!(!a.admit(1,core(2,0))); assert_eq!(a.records[1].reason,Reason::Unknown);
    let mut a=Admission::<4>::new(true); a.bsp(core(0,0));
    assert!(!a.admit(1,Core::unknown(2))); assert_eq!(a.records[1].reason,Reason::Unknown);
    let mut a=Admission::<4>::new(false); a.bsp(Core::unknown(0));
    assert!(a.admit(1,Core::unknown(1)) && a.admit(2,core(0,1)));
    // MT=0 uses Aff0 as core; MT=1 uses it as thread. Preserve upper affinity.
    let mut a=Admission::<4>::new(true); a.bsp(Core::arm(0));
    assert!(a.admit(1,Core::arm(1)));
    let mut a=Admission::<4>::new(true); a.bsp(Core::arm(1<<24));
    assert!(!a.admit(1,Core::arm((1<<24)|1)));
    assert!(a.admit(2,Core::arm((1<<24)|(1<<8))));
    assert!(a.admit(3,Core::arm((1<<24)|(1<<32))));
    assert_eq!(Core::arm(1<<30).base,Core::arm(0).base); // U is not affinity
    // Random discovery order, SMT4, multiple packages: never admit overlap.
    let mut seed=1u64;
    for _ in 0..200 {
        let mut ids:Vec<_>=(1..64u32).collect();
        for i in (1..ids.len()).rev() { seed=seed.wrapping_mul(6364136223846793005).wrapping_add(1); ids.swap(i,(seed as usize)%(i+1)); }
        let mut a=Admission::<64>::new(true); a.bsp(core(0,2));
        for (i,id) in ids.into_iter().enumerate() { a.admit(i+1,core(id,2)); }
        let online:Vec<_>=a.records.iter().filter(|r|r.reason==Reason::Online).collect();
        assert_eq!(online.len(),16);
        for i in 0..online.len() { for j in 0..i { assert!(!online[i].core.overlaps(online[j].core)); } }
    }
    println!("SMT topology: package/core ranges, hybrid widths, arbitrary order, unknown fail-closed, MPIDR.MT, force/off policy and SMT4 admission passed");
}
