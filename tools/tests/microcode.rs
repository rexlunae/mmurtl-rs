#[path = "../../src/microcode/mod.rs"] mod microcode;
use microcode::{intel, amd, Error};
fn put(b: &mut [u8], o: usize, v: u32) { b[o..o+4].copy_from_slice(&v.to_le_bytes()); }
fn get(b: &[u8], o: usize) -> u32 { u32::from_le_bytes(b[o..o+4].try_into().unwrap()) }
fn checksum(b: &mut [u8], field: usize) {
    put(b, field, 0); let sum = b.chunks_exact(4).fold(0u32, |s, x| s.wrapping_add(u32::from_le_bytes(x.try_into().unwrap()))); put(b, field, 0u32.wrapping_sub(sum));
}
fn intel_patch(rev: u32) -> Vec<u8> {
    let mut p = vec![0; 2048]; put(&mut p,0,1); put(&mut p,4,rev); put(&mut p,12,0x306a9); put(&mut p,20,1); put(&mut p,24,2); checksum(&mut p,16); p
}
fn amd_patch() -> Vec<u8> {
    let mut p=vec![0;12+8+64];put(&mut p,0,0x414d44);put(&mut p,12,1);put(&mut p,16,64);put(&mut p,24,0x0830107b);p[44..46].copy_from_slice(&0x8310u16.to_le_bytes());p
}
fn main() {
    let p=intel_patch(10);assert_eq!(intel(&p,0x306a9,2,0).unwrap().unwrap().revision,10);
    assert!(intel(&p,0x306a9,1,0).unwrap().is_none()); assert!(intel(&p,0x306a8,2,0).unwrap().is_none());assert!(intel(&p,0x306a9,2,10).unwrap().is_none());
    let mut both=p.clone();both.extend(intel_patch(12));assert_eq!(intel(&both,0x306a9,2,0).unwrap().unwrap().revision,12);
    for n in [1,47,2047] { assert!(intel(&p[..n],0x306a9,2,0).is_err()); }
    let mut bad=p.clone();bad[100]^=1;assert_eq!(intel(&bad,0x306a9,2,0).unwrap_err(),Error::Checksum);
    let mut bad=p.clone();put(&mut bad,28,u32::MAX);assert!(intel(&bad,0x306a9,2,0).is_err());
    let mut prerequisite=p.clone();put(&mut prerequisite,40,5);checksum(&mut prerequisite,16);assert!(intel(&prerequisite,0x306a9,2,4).unwrap().is_none());assert!(intel(&prerequisite,0x306a9,2,5).unwrap().is_some());
    // Extended signature checksums, with a different CPU/platform match.
    let mut ext=p.clone();put(&mut ext,28,2000);put(&mut ext,32,2080);checksum(&mut ext,16);
    let original=get(&ext,12).wrapping_add(get(&ext,24)).wrapping_add(get(&ext,16));
    ext.resize(2080,0);put(&mut ext,2048,1);put(&mut ext,2068,0x50654);put(&mut ext,2072,4);put(&mut ext,2076,original.wrapping_sub(0x50654+4));checksum(&mut ext[2048..],4);
    assert!(intel(&ext,0x50654,4,0).unwrap().is_some());ext[2076]^=1;assert!(intel(&ext,0x50654,4,0).is_err());
    let a=amd_patch();assert_eq!(amd(&a,0x00830f10,0).unwrap().unwrap().revision,0x0830107b);assert!(amd(&a,0x00830f11,0).unwrap().is_none());assert!(amd(&a,0x00830f10,0x0830107b).unwrap().is_none());
    let mut bad=a.clone();put(&mut bad,16,u32::MAX);assert!(amd(&bad,0x00830f10,0).is_err());
    // Older-family equivalence table and chipset-specific rejection.
    let mut old=vec![0;12+16+8+64];put(&mut old,0,0x414d44);put(&mut old,8,16);put(&mut old,12,0x600f20);old[24..26].copy_from_slice(&0x6012u16.to_le_bytes());put(&mut old,28,1);put(&mut old,32,64);put(&mut old,40,0x0600063e);old[60..62].copy_from_slice(&0x6012u16.to_le_bytes());assert!(amd(&old,0x600f20,0).unwrap().is_some());put(&mut old,52,1);assert!(amd(&old,0x600f20,0).unwrap().is_none());
    assert!(microcode::amd_policy::transition_allowed(0x0830107b,0x0830107c));assert!(!microcode::amd_policy::transition_allowed(0x0830107c,0x0830107d));assert!(microcode::amd_policy::transition_allowed(0x0830107d,0x0830107e));
    for file in std::env::args().skip(1) {
        let blob=std::fs::read(&file).unwrap();
        if get(&blob,0)==1 {
            let pf=get(&blob,24);let minimum=get(&blob,40);let cpu=get(&blob,12);let platform=pf & pf.wrapping_neg();
            assert!(intel(&blob,cpu,platform,minimum).unwrap().is_some(),"vendor Intel sample {file}");
        } else {
            let cpu=get(&blob,12);let result=amd(&blob,cpu,0).unwrap();assert!(result.is_some(),"vendor AMD sample {file}");
        }
        println!("vendor sample validated: {file}");
    }
    println!("microcode: legacy/extended Intel, AMD/Zen matching, prerequisites, corrupt/truncated input and signing boundaries passed");
}
