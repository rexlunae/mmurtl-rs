use crate::{elf,user_layout as layout};
fn put16(b:&mut[u8],at:usize,value:u16){b[at..at+2].copy_from_slice(&value.to_le_bytes());}
fn put32(b:&mut[u8],at:usize,value:u32){b[at..at+4].copy_from_slice(&value.to_le_bytes());}
fn put64(b:&mut[u8],at:usize,value:u64){b[at..at+8].copy_from_slice(&value.to_le_bytes());}
fn ph(b:&mut[u8],index:usize,kind:u32,flags:u32,offset:u64,va:u64,size:u64){
    let at=64+index*56;put32(b,at,kind);put32(b,at+4,flags);put64(b,at+8,offset);put64(b,at+16,va);put64(b,at+32,size);put64(b,at+40,size);
}
fn pie(machine:u16)->Vec<u8>{
    let mut b=vec![0;0x5000];b[..4].copy_from_slice(b"\x7fELF");b[4]=2;b[5]=1;b[6]=1;
    put16(&mut b,16,3);put16(&mut b,18,machine);put32(&mut b,20,1);put64(&mut b,24,0x1000);put64(&mut b,32,64);put16(&mut b,52,64);put16(&mut b,54,56);put16(&mut b,56,5);
    ph(&mut b,0,1,5,0x1000,0x1000,0x1000);
    ph(&mut b,1,1,4,0x2000,0x2000,0x1000);
    ph(&mut b,2,1,6,0x3000,0x3000,0x2000);
    ph(&mut b,3,2,6,0x3100,0x3100,64);
    ph(&mut b,4,0x6474e552,4,0x3000,0x3000,0x1000);
    for (index,(tag,value)) in [(7,0x2000),(8,24),(9,24),(0,0)].into_iter().enumerate(){put64(&mut b,0x3100+16*index,tag);put64(&mut b,0x3108+16*index,value);}
    put64(&mut b,0x2000,0x4000);put64(&mut b,0x2008,if machine==62 {8}else{1027});put64(&mut b,0x2010,0x1000);b
}
fn parse(b:&[u8],machine:u16)->Result<elf::Image,&'static str>{elf::parse(b,machine,layout::BASE,layout::IMAGE_LIMIT)}
#[test]
fn pie_validation_relocations_and_relro(){
    for machine in [62,183]{
        let valid=pie(machine);let parsed=parse(&valid,machine).unwrap();
        assert!(parsed.pie && parsed.readonly_page(0x3000) && !parsed.readonly_page(0x4000));
        assert_eq!(parsed.relocations(&valid).collect::<Vec<_>>(),[(0x4000,0x1000)]);
        let mut unsorted=valid.clone();put64(&mut unsorted,0x3118,48);
        put64(&mut unsorted,0x2000,0x4008);
        put64(&mut unsorted,0x2018,0x4000);put64(&mut unsorted,0x2020,if machine==62 {8}else{1027});put64(&mut unsorted,0x2028,0x2000);
        assert_eq!(parse(&unsorted,machine).unwrap().relocations(&unsorted).collect::<Vec<_>>(),[(0x4008,0x1000),(0x4000,0x2000)]);
        let mut legacy=valid.clone();put16(&mut legacy,16,2);put16(&mut legacy,56,1);
        put64(&mut legacy,24,layout::BASE+0x1000);put64(&mut legacy,64+16,layout::BASE+0x1000);
        assert!(!parse(&legacy,machine).unwrap().pie);
        for (at,value) in [(0x2000,0x1000),(0x2000,u64::MAX-7),(0x2000,0x5000),(0x2008,1),(0x2008,(1<<32)|8),(0x2010,u64::MAX),(0x3118,25),(0x3108,0x6000)] {
            let mut bad=valid.clone();put64(&mut bad,at,value);assert!(parse(&bad,machine).is_err(),"mutation {at:x}={value:x}");
        }
        let mut bad=valid.clone();put64(&mut bad,0x3100,1);assert!(parse(&bad,machine).is_err());
        let mut bad=valid.clone();put64(&mut bad,0x3130,10);assert!(parse(&bad,machine).is_err());
        let mut bad=valid.clone();put64(&mut bad,64+56+16,0x1800);assert!(parse(&bad,machine).is_err());
        let mut bad=valid.clone();put32(&mut bad,64+4,7);assert!(parse(&bad,machine).is_err());
        let mut bad=valid.clone();put64(&mut bad,64+40,elf::MAX_IMAGE+1);assert!(parse(&bad,machine).is_err());
        let mut bad=valid.clone();put64(&mut bad,64+4*56+16,0x3001);assert!(parse(&bad,machine).is_err());
        let mut bad=valid.clone();put64(&mut bad,24,0x3000);assert!(parse(&bad,machine).is_err());
        for end in [0,63,64,300,4096] {assert!(parse(&valid[..end],machine).is_err());}
    }
}
#[test]
fn randomized_layout_bounds(){
    let mut biases=std::collections::BTreeSet::new();
    for seed in [0,1,0x1ffffff,u64::MAX] {
        let l=layout::choose(seed,seed,seed);
        assert!(l.bias>=layout::BASE+layout::PAGE && l.bias+elf::MAX_IMAGE<layout::IMAGE_LIMIT);
        assert!(l.stack_edge-layout::STACK_PAGES*layout::PAGE>layout::IMAGE_LIMIT);
        assert!(l.stack_edge+layout::PAGE<layout::END);
        assert_eq!(l.bias%layout::PAGE,0);assert_eq!(l.stack_pointer%16,0);
        assert!(l.stack_pointer>=l.stack_edge-layout::STACK_PAGES*layout::PAGE && l.stack_pointer<l.stack_edge-32);
        for alignment in [4096,8192,65536] {
            let bias=l.image_bias(alignment);assert_eq!(bias%alignment,0);
            assert!(bias>layout::BASE && bias+elf::MAX_IMAGE<layout::IMAGE_LIMIT);
            assert!((1u64<<25)/(alignment/layout::PAGE)>=(1<<21));
        }
    }
    for code in 0..10000 {assert!(biases.insert(layout::choose(code,0,0).bias));}
}
#[test]
fn built_runtime_images(){
    if std::env::var_os("MMURTL_USER_TESTS").is_none() { return; }
    let root=std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap();
    for (target,machine) in [("x86_64-unknown-none",62),("aarch64-unknown-none-softfloat",183)] {
        for name in ["hello","sieve","canary","relro"] {
            let bytes=std::fs::read(root.join("user/target").join(target).join("release").join(name)).unwrap();
            let image=parse(&bytes,machine).unwrap();
            assert!(image.pie && image.relro.is_some());
            assert!(image.relocations(&bytes).count()>0);
        }
    }
}
