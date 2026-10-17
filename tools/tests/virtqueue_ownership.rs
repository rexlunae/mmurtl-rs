extern crate alloc;
#[path = "../../src/virtio/ownership.rs"] mod ownership;
use ownership::Ownership;
fn main() {
    let mut q = Ownership::new(8);
    let first = q.reserve(3).unwrap(); let second = q.reserve(2).unwrap();
    assert_eq!(first, 0); assert_eq!(second, 3); assert_eq!(q.next(first), 1);
    assert_eq!(q.complete(first as u32, 3, Some(first)), Some(first));
    assert_eq!(q.complete(first as u32, 3, None), None); // double-free
    assert!(q.broken()); assert_eq!(q.reserve(1), None); assert_eq!(q.complete(second as u32, 2, None), None);
    for (id, count, expected) in [(0x10000, 1, None), (8, 1, None), (1, 1, None), (0, 2, None), (0, 1, Some(1))] {
        let mut q = Ownership::new(8); q.reserve(1).unwrap();
        assert_eq!(q.complete(id, count, expected), None); assert!(q.broken());
    }
    let mut q = Ownership::new(8); assert_eq!(q.reserve(65536), None);
    assert_eq!(q.reserve(0), None); let head = q.reserve(8).unwrap(); assert_eq!(q.reserve(1), None);
    assert_eq!(q.complete(head as u32, 8, None), Some(head)); assert!(q.reserve(8).is_some());
    for _ in 0..1000 { let mut q = Ownership::new(4); let a = q.reserve(2).unwrap(); let b = q.reserve(2).unwrap(); q.complete(b as u32, 2, None).unwrap(); q.complete(a as u32, 2, None).unwrap(); assert!(q.reserve(4).is_some()); }
    println!("virtqueue ownership: valid chains, duplicate/invalid IDs, wrong lengths, poison and reuse passed");
}
