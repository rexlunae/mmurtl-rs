#[path="../../src/memory/slab.rs"] mod slab;
use std::alloc::{alloc_zeroed,dealloc,Layout};
use slab::{Error,Slabs};
struct Mock {heap:Box<Slabs>,base:usize,next:usize,end:usize,layout:Layout,random:usize}
impl Mock {
    fn new()->Self {
        let layout=Layout::from_size_align(64<<20,16<<20).unwrap();
        let base=unsafe {alloc_zeroed(layout)} as usize;assert_ne!(base,0);
        let mut heap=Box::new(Slabs::new());heap.seed(0x9ad64351bcacfe37);
        Self {heap,base,next:base,end:base+layout.size(),layout,random:19}
    }
    fn allocate(&mut self,layout:Layout,domain:usize)->usize {
        if let Some(pointer)=unsafe {self.heap.allocate(layout,domain)}.unwrap() {return pointer;}
        let req=slab::request(layout,domain).unwrap();
        let base=(self.next+req.align-1)&!(req.align-1);self.next=base+req.span;assert!(self.next<=self.end);
        let mut random=self.random;
        unsafe {self.heap.add(base,req,|| {random^=random<<13;random^=random>>7;random^=random<<17;random})}.unwrap();
        self.random=random;
        unsafe {self.heap.allocate(layout,domain)}.unwrap().unwrap()
    }
    fn free(&mut self,p:usize,l:Layout,d:usize) {unsafe {self.heap.deallocate(p,l,d)}.unwrap();}
}
impl Drop for Mock {fn drop(&mut self) {unsafe {dealloc(self.base as *mut u8,self.layout);}}}
fn main() {
    let mut mock=Mock::new();
    let mut pointers=Vec::new();
    for domain in 0..slab::DOMAINS {
        for size in [0,1,16,17,31,128,500,4096,65536] {
            let layout=Layout::from_size_align(size,64).unwrap();
            for _ in 0..12 {
                let p=mock.allocate(layout,domain);assert_eq!(p%layout.align(),0);
                assert!(unsafe {std::slice::from_raw_parts(p as *const u8,size)}.iter().all(|b|*b==0));
                unsafe {std::ptr::write_bytes(p as *mut u8,0xa5,size);}
                pointers.push((p,layout,domain));
            }
        }
    }
    let mut unique:Vec<_>=pointers.iter().map(|x|x.0).collect();unique.sort_unstable();unique.dedup();assert_eq!(unique.len(),pointers.len());
    assert!(pointers[..12].windows(2).any(|p|p[0].0>p[1].0),"new slab order stayed ascending");
    for &(p,l,d) in pointers.iter().rev() {mock.free(p,l,d);}
    assert_eq!(mock.heap.live_bytes,0);assert_eq!(mock.heap.allocations,0);
    let reserved=mock.heap.reserved_bytes;
    for _ in 0..1000 {
        for &(_,l,d) in &pointers {
            let p=mock.allocate(l,d);assert!(unsafe {std::slice::from_raw_parts(p as *const u8,l.size())}.iter().all(|b|*b==0));mock.free(p,l,d);
        }
    }
    assert_eq!(mock.heap.reserved_bytes,reserved,"churn grew cache backing");
    let isolated=Layout::from_size_align(256,16).unwrap();
    let generic=mock.allocate(isolated,0);mock.free(generic,isolated,0);
    let typed=mock.allocate(isolated,1);assert_ne!(generic,typed);
    let reused=mock.allocate(isolated,0);assert_eq!(generic,reused);
    mock.free(typed,isolated,1);mock.free(reused,isolated,0);
    let l=Layout::from_size_align(64,64).unwrap();let p=mock.allocate(l,1);
    assert_eq!(unsafe {mock.heap.deallocate(p,l,0)},Err(Error::Layout));
    assert_eq!(unsafe {mock.heap.deallocate(p+1,l,1)},Err(Error::Alignment));
    assert_eq!(unsafe {mock.heap.deallocate(1,l,1)},Err(Error::Range));
    mock.free(p,l,1);
    assert_eq!(unsafe {mock.heap.deallocate(p,l,1)},Err(Error::DoubleFree));
    assert!(unsafe {std::slice::from_raw_parts((p+8) as *const u8,56)}.iter().all(|b|*b==0));
    // Corrupt an encoded next pointer. Pop must reject it before following
    // the forged pointer or changing the allocation bitmap.
    unsafe {(p as *mut usize).write(0);}
    assert!(unsafe {mock.heap.allocate(l,1)}.is_err());
    assert_eq!(mock.heap.allocations,0);
    for (size,align) in [(17,8192),(1<<24,1<<24)] {
        let l=Layout::from_size_align(size,align).unwrap();let p=mock.allocate(l,2);assert_eq!(p%align,0);mock.free(p,l,2);
    }
    assert_eq!(slab::request(Layout::from_size_align((1<<24)+1,8).unwrap(),0).err(),Some(Error::Layout));
    println!("slabs: randomized slots, alignment, domain/size isolation, scrub/reuse, 1,000 steady-state waves, double-free and forged-link rejection passed");
}
