#[path="../../src/security/return_policy.rs"] mod policy;
fn main() {
    for field in 0..16 {
        for shift in [4,8] {assert_eq!(policy::pac_signature(field<<shift,0).is_some(),(1..=6).contains(&field));}
        assert_eq!(policy::pac_signature(0,field<<12).is_some(),(1..=6).contains(&field));
    }
    assert_eq!(policy::pac_signature(1<<24|1<<28,0),None);
    assert_eq!(policy::pac_signature(1<<4|1<<8,0),None);
    assert_eq!(policy::pac_signature(1<<4,1<<12),None);
    assert_ne!(policy::pac_signature(1<<4,0),policy::pac_signature(1<<8,0));
    for value in [0,1,255,256,0x123456789abcdef0,u64::MAX] {
        let canary=policy::canary(value);assert_ne!(canary,0);assert_eq!(canary&255,0);
    }
    println!("return policy: PAC algorithms, reserved fields, heterogeneous signatures and nonzero terminated canaries passed");
}
