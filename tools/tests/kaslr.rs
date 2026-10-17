#![allow(dead_code)]
#[path="../../src/security/kaslr.rs"] mod kaslr;
fn main() {
    for word in (0..100_000u64).chain([u64::MAX,1<<63,0xaaaaaaaaaaaaaaaa,(1<<21)-2,(1<<21)-8]) {
        let (linear,image) = kaslr::arm_layout([word,word.rotate_left(19),word.rotate_left(37)]);
        assert_eq!(linear & ((1<<39)-1),0);
        assert_eq!(image & ((1<<21)-1),0);
        assert!(linear >= 0xffff000000000000 && image >= 0xffff000000000000);
        // Linear window and independently chosen image placement never overlap.
        assert!(linear + (1<<39) <= image);
        for load in [0x200000u64,0x40200000,0x41001000,0x7f00000000] {
            let offset = image - (load & !((2 << 20) - 1));
            assert_eq!(load + offset, image + (load & ((2 << 20) - 1)));
            assert_eq!(offset & 4095,0);
        }
        let heap = kaslr::amd_heap(word);
        let ist = kaslr::amd_ist(word);
        assert!(heap + (1 << 36) < ist);
        assert!(ist > kaslr::AMD_DYNAMIC_END && ist + (64 * 4 * 9 * 4096) < 0xffff_cd00_0000_0000);
        assert!(heap > kaslr::AMD_DYNAMIC_END);
        assert_eq!(heap & ((1<<21)-1),0);
        assert!(heap + (1<<30) < 0xffff_c500_0000_0000);
        assert!(heap+(4<<20)<=kaslr::AMD_HEAP_BASE+(4<<40));
        let arm_heap=kaslr::arm_heap(word);
        assert!(arm_heap>=kaslr::ARM_HEAP_BASE);
        assert!(arm_heap+(16<<20)<=kaslr::ARM_HEAP_BASE+(4<<40));
        assert!(image+(1<<38)<kaslr::ARM_HEAP_BASE);
        assert!(arm_heap+(16<<20)<0xffff_e000_0000_0000);
    }
    assert_ne!(kaslr::arm_layout([0,0,0]),kaslr::arm_layout([1,1,1]));
    assert_ne!(kaslr::amd_heap(0),kaslr::amd_heap(1));
    println!("KASLR placement: alignment, canonical ranges, heap separation and window isolation passed");
}
