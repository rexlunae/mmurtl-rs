//! Baseline opcodes and layout constraints for AMD safe-return sequences.
use super::{SpecCaps,Vulnerability as V};
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
#[repr(u8)]
pub enum Class {Generic=0,Zen12=1,Zen34=2,Unknown=3}
pub fn class(caps:SpecCaps)->Class {
    if caps.vendor==*b"GenuineIntel" {return Class::Generic;}
    if caps.vendor!=*b"AuthenticAMD" {return Class::Unknown;}
    let base=(caps.identity>>8)&15;
    let family=base+if base==15 {(caps.identity>>20)&255} else {0};
    match family {0x17=>Class::Zen12,0x19=>Class::Zen34,_=>Class::Unknown}
}
#[derive(Clone,Copy,Debug)]
pub struct Sites {pub untrain:usize,pub safe:usize,pub entry:usize}
pub fn sites(kind:Class)->Sites {
    match kind {Class::Zen12=>Sites {untrain:62,safe:64,entry:128},
        Class::Zen34=>Sites {untrain:0,safe:0x104104,entry:0x10410c},
        _=>panic!("no dynamic return sequence")}
}
pub fn jump(from:u64,to:u64)->Option<[u8;8]> {
    let displacement=(to as i128)-(from as i128)-5;
    let displacement=i32::try_from(displacement).ok()?.to_le_bytes();
    Some([0xe9,displacement[0],displacement[1],displacement[2],displacement[3],0xcc,0xcc,0xcc])
}
fn call(from:usize,to:usize)->[u8;5] {
    let d=((to as isize)-(from as isize)-5) as i32;
    let b=d.to_le_bytes();[0xe8,b[0],b[1],b[2],b[3]]
}
/// Each slice is exclusively owned, writable, NX physical backing. Its
/// mutable borrows end before the caller seals aliases and maps code RX.
pub fn build(kind:Class,first:&mut [u8],second:&mut [u8])->Sites {
    assert_eq!(first.len(),4096);assert_eq!(second.len(),4096);
    first.fill(0xcc);second.fill(0xcc);let s=sites(kind);
    let safe=[0x48,0x8d,0x64,0x24,0x08,0xc3,0xcc];
    match kind {
        Class::Zen12=>{
            first[62..64].copy_from_slice(&[0x48,0xb8]);
            first[64..71].copy_from_slice(&safe);first[71]=0xcc;
            first[72..75].copy_from_slice(&[0x0f,0xae,0xe8]);
            first[75..80].copy_from_slice(&call(75,s.safe));
            first[80..82].copy_from_slice(&[0x0f,0x0b]);
            first[128..133].copy_from_slice(&call(128,s.safe));
            first[133..135].copy_from_slice(&[0x0f,0x0b]);
        }
        Class::Zen34=>{
            first[..5].copy_from_slice(&[0x66,0x90,0x0f,0xae,0xe8]);
            first[5..13].copy_from_slice(&jump(5,s.entry as u64).unwrap());
            second[0x104..0x10b].copy_from_slice(&safe);
            second[0x10c..0x111].copy_from_slice(&call(s.entry,s.safe));
            second[0x111..0x113].copy_from_slice(&[0x0f,0x0b]);
        }
        _=>unreachable!(),
    }
    s
}
pub fn microcode_return_barrier(caps:super::CommonCaps)->bool {
    caps.features&(super::feature::IBPB|super::feature::IBPB_RET)==(super::feature::IBPB|super::feature::IBPB_RET)
        ||caps.not_affected&V::Srso.bit()!=0
}
