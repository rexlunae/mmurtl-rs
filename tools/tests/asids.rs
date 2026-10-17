#[path = "../../src/arch/arm64/asid_pool.rs"] mod pool;
use pool::Asids;
fn main() {
    let mut p = Asids::<16, 4>::new(2); // force frequent 3-tag rollover
    for root in 1..=6 { assert!(p.register(root)); }
    let a=p.activate(0,1).0.unwrap();let b=p.activate(1,2).0.unwrap();
    assert_ne!(a,b);assert!(!p.activate(2,3).1);p.activate(2,0);
    let (d,flush)=p.activate(2,4);assert!(flush);assert_ne!(d.unwrap(),a);assert_ne!(d.unwrap(),b);
    assert_eq!(p.activate(0,1),(Some(a),false));assert_eq!(p.activate(1,2),(Some(b),false));
    // All tags active: no aliasing fallback is allowed.
    let (tag,flush)=p.activate(3,5);assert!(flush);assert!(tag.is_none());
    p.activate(2,0);let (tag,flush)=p.activate(3,5);assert!(flush);assert_ne!(tag.unwrap(),a);assert_ne!(tag.unwrap(),b);
    p.activate(0,0);p.release(1);assert!(p.register(7));
    let mut q=Asids::<2, 1>::new(8);q.configure(16);
    for i in 0..70000 {
        assert!(q.register(10));let (tag,_) = q.activate(0,10);assert_ne!(tag.unwrap(),0);
        q.activate(0,0);q.release(10);
        if i==65534 { assert_eq!(q.generation(),1); }
    }
    assert!(q.generation()>1);
    let mut q=Asids::<2, 1>::new(8);
    for _ in 0..1000 { assert!(q.register(10));q.activate(0,10);q.activate(0,0);q.release(10); }
    assert!(q.generation()>1);
    println!("ASIDs: 8/16-bit rollover, live CPU reservations, exhaustion and root reuse passed");
}
