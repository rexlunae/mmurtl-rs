#![allow(dead_code)]
#[path="../../src/security/types.rs"] mod types;
#[path="../../src/security/arm_pan.rs"] mod pan;
use types::feature as F;
fn main() {
    assert_eq!(pan::features(0,0),0);
    assert_eq!(pan::features(1<<20,0),F::PAN);
    assert_eq!(pan::features(2<<20,1<<4),F::PAN|F::UAO);
    assert_eq!(pan::features(3<<20,1<<4),F::PAN|F::EPAN|F::UAO);
    assert_eq!(pan::features(15<<20,15<<4),0);
    let other=(15u64<<28)|(1<<12)|(15<<6);
    for mode in [0,5] { for hardware in [false,true] {
        let old=other|mode|(1<<22)|(1<<23);
        let new=pan::return_spsr(old,hardware);
        assert_eq!(new & (1<<23),0);
        assert_eq!(new & (1<<22)!=0,hardware && mode!=0);
        assert_eq!(new & !((1<<22)|(1<<23)),old & !((1<<22)|(1<<23)));
    } }
    println!("ARM PAN: feature gates, reserved fields, UAO clearing and saved-PSTATE flags passed");
}
