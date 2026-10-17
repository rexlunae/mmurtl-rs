#[path="../../src/memory/stack_layout.rs"] mod layout;
use layout::*;
fn checked_bottom(frame:u64,next:u64)->Option<u64> { layout::bottom(frame,BASE+STRIDE,next) }
fn main() {
    assert_eq!(layout::bottom(BASE+STRIDE+PAGE,BASE+100*STRIDE,BASE+101*STRIDE),None);
    assert_eq!(checked_bottom(BASE,BASE+STRIDE),None);
    for slot in [1,2,63,1024,(LIMIT-BASE)/STRIDE-1] {
        let guard=BASE+slot*STRIDE; let start=guard+PAGE; let next=guard+STRIDE;
        assert_eq!(checked_bottom(guard,next),None);
        assert_eq!(checked_bottom(start-1,next),None);
        assert_eq!(checked_bottom(start,next),Some(start));
        assert_eq!(checked_bottom(start+SIZE-1,next),Some(start));
        assert_eq!(checked_bottom(start+SIZE,next),None);
        assert_eq!(checked_bottom(next-1,next),None);
        assert_eq!(checked_bottom(start,guard),None);
        for offset in (0..1024).step_by(16) {
            let frame=start+SIZE-272;
            assert!(frame-offset>start);
            assert_eq!(checked_bottom(frame,next),Some(start));
        }
    }
    for outside in [0,0x640000001000,u64::MAX,BASE-1,LIMIT] { assert_eq!(checked_bottom(outside,LIMIT),None); }
    println!("stack layout: lower/upper guards, live reservation bounds and aligned entry gaps passed");
}
