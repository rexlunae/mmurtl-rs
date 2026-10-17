#![allow(dead_code)]
#[path="../../src/security/types.rs"] mod types;
pub use types::*;
#[path="../../src/security/return_thunk.rs"] mod thunk;
use thunk::Class;
fn main() {
    assert_eq!(thunk::class(SpecCaps {vendor:*b"AuthenticAMD",identity:0x800f10,..SpecCaps::default()}),Class::Zen12);
    assert_eq!(thunk::class(SpecCaps {vendor:*b"AuthenticAMD",identity:0xa00f10,..SpecCaps::default()}),Class::Zen34);
    assert_eq!(thunk::class(SpecCaps {vendor:*b"GenuineIntel",..SpecCaps::default()}),Class::Generic);
    assert_eq!(thunk::class(SpecCaps::default()),Class::Unknown);
    for kind in [Class::Zen12,Class::Zen34] {
        let mut first=[0;4096];let mut second=[0;4096];let s=thunk::build(kind,&mut first,&mut second);
        let image=if kind==Class::Zen12 {&first} else {&second};
        assert_eq!(&image[s.safe&4095..(s.safe&4095)+7],&[0x48,0x8d,0x64,0x24,8,0xc3,0xcc]);
        let call=s.entry&4095;assert_eq!(image[call],0xe8);
        let displacement=i32::from_le_bytes(image[call+1..call+5].try_into().unwrap()) as isize;
        assert_eq!((s.entry as isize+5+displacement) as usize,s.safe);
        if kind==Class::Zen12 {assert_eq!(s.safe%64,0);assert_eq!(s.untrain+2,s.safe);assert_eq!(&first[62..64],&[0x48,0xb8]);}
        else {assert_eq!(s.untrain,0);assert_eq!(s.safe,0x104104);assert_eq!(s.safe&(1<<2|1<<8|1<<14|1<<20),1<<2|1<<8|1<<14|1<<20);}
    }
    assert!(thunk::jump(0,1<<40).is_none());
    assert_eq!(thunk::jump(0x1000,0x2000).unwrap()[0],0xe9);
    assert!(!thunk::microcode_return_barrier(CommonCaps {features:feature::IBPB,not_affected:0}));
    assert!(thunk::microcode_return_barrier(CommonCaps {features:feature::IBPB|feature::IBPB_RET,not_affected:0}));
    println!("return thunks: CPU classes, MOVABS overlap/cacheline, Zen3/4 2-MiB alias bits, safe RET traps, relative branches and microcode gates passed");
}
