//! Allocation-free slab bookkeeping shared by the kernel and host tests.
//! Callers serialize access and supply live, exclusively owned backing RAM.
use core::alloc::Layout;
pub const PAGE:usize=4096;
pub const DOMAINS:usize=3;
pub const MIN_SHIFT:usize=4;
pub const MAX_SHIFT:usize=24;
const CLASSES:usize=MAX_SHIFT-MIN_SHIFT+1;
pub const MAX_SLABS:usize=4096;
#[derive(Debug,Clone,Copy,PartialEq,Eq)]
pub enum Error {Layout,Range,Alignment,DoubleFree,Corrupt,Exhausted}
#[derive(Clone,Copy)]
struct Slab {
    base:usize, span:usize, stride:usize, cache:usize,
    next:usize, head:usize, free:usize, live:[u64;4],
}
impl Slab {
    const EMPTY:Self=Self {base:0,span:0,stride:0,cache:0,next:0,head:0,free:0,live:[0;4]};
    fn slot(&self,pointer:usize)->Result<usize,Error> {
        let offset=pointer.checked_sub(self.base).filter(|v|*v<self.span).ok_or(Error::Range)?;
        if offset%self.stride!=0 {return Err(Error::Alignment);}
        Ok(offset/self.stride)
    }
    fn is_live(&self,slot:usize)->bool {self.live[slot/64]&(1<<(slot%64))!=0}
    fn validate_free(&self,pointer:usize)->Result<(),Error> {
        let slot=self.slot(pointer)?;
        if self.is_live(slot) {return Err(Error::Corrupt);}
        Ok(())
    }
}
#[derive(Clone,Copy,Debug)]
pub struct Request {pub span:usize,pub align:usize,cache:usize,stride:usize}
pub fn request(layout:Layout,domain:usize)->Result<Request,Error> {
    if domain>=DOMAINS {return Err(Error::Layout);}
    let stride=layout.size().max(layout.align()).max(1<<MIN_SHIFT).checked_next_power_of_two().ok_or(Error::Layout)?;
    if stride>1<<MAX_SHIFT {return Err(Error::Layout);}
    let class=stride.trailing_zeros() as usize-MIN_SHIFT;
    Ok(Request {span:stride.max(PAGE),align:stride.max(PAGE),cache:domain*CLASSES+class,stride})
}
pub struct Slabs {
    pages:[Slab;MAX_SLABS], used:usize, lists:[usize;DOMAINS*CLASSES],
    secret:usize, pub live_bytes:usize,pub reserved_bytes:usize,pub allocations:usize,
}
impl Slabs {
    pub const fn new()->Self {Self {pages:[Slab::EMPTY;MAX_SLABS],used:0,lists:[0;DOMAINS*CLASSES],secret:0,live_bytes:0,reserved_bytes:0,allocations:0}}
    pub fn seed(&mut self,secret:usize) {assert_eq!(self.used,0);self.secret=secret;}
    pub fn has_metadata(&self)->bool {self.used<MAX_SLABS}
    /// Backing spans must be monotonically allocated and never reassigned
    /// to a different cache. Random draws come from the kernel CSPRNG.
    pub unsafe fn add(&mut self,base:usize,req:Request,mut random:impl FnMut()->usize)->Result<(),Error> {
        if !self.has_metadata() {return Err(Error::Exhausted);}
        if req.span!=req.stride.max(PAGE)||req.align!=req.span||req.cache>=self.lists.len() {return Err(Error::Layout);}
        if base==0||base%req.align!=0 || base.checked_add(req.span).is_none() {return Err(Error::Alignment);}
        if self.used!=0 {
            let last=&self.pages[self.used-1];
            if base<last.base+last.span {return Err(Error::Range);}
        }
        let count=req.span/req.stride;
        let mut order=[0u16;PAGE/(1<<MIN_SHIFT)];
        for (i,slot) in order[..count].iter_mut().enumerate() {*slot=i as u16;}
        for i in (1..count).rev() {let j=random()%(i+1);order.swap(i,j);}
        for (i,slot) in order[..count].iter().enumerate() {
            let address=base+*slot as usize*req.stride;
            let next=if i+1<count {base+order[i+1] as usize*req.stride} else {0};
            (address as *mut usize).write_volatile(next^self.secret^address);
        }
        self.pages[self.used]=Slab {base,span:req.span,stride:req.stride,cache:req.cache,
            next:self.lists[req.cache],head:base+order[0] as usize*req.stride,free:count,live:[0;4]};
        self.used+=1;self.lists[req.cache]=self.used;self.reserved_bytes+=req.span;
        Ok(())
    }
    /// None requests a new slab from the architecture's backing allocator.
    pub unsafe fn allocate(&mut self,layout:Layout,domain:usize)->Result<Option<usize>,Error> {
        let req=request(layout,domain)?;
        let mut cursor=self.lists[req.cache];
        let mut visited=0;
        while cursor!=0 {
            if cursor>self.used || visited>=self.used {return Err(Error::Corrupt);}
            visited+=1;
            let page=&mut self.pages[cursor-1];
            if page.cache!=req.cache {return Err(Error::Corrupt);}
            if page.free!=0 {
                let pointer=page.head;
                page.validate_free(pointer)?;
                let next=(pointer as *const usize).read_volatile()^self.secret^pointer;
                if next!=0 {page.validate_free(next)?;if next==pointer {return Err(Error::Corrupt);}}
                if (page.free==1)!=(next==0) {return Err(Error::Corrupt);}
                let slot=page.slot(pointer)?;
                page.live[slot/64]|=1<<(slot%64);page.head=next;page.free-=1;
                // Free-list linkage is the only nonzero word in a free slot.
                (pointer as *mut usize).write_volatile(0);
                self.live_bytes+=page.stride;self.allocations+=1;
                return Ok(Some(pointer));
            }
            if page.head!=0 {return Err(Error::Corrupt);}
            cursor=page.next;
        }
        Ok(None)
    }
    pub unsafe fn deallocate(&mut self,pointer:usize,layout:Layout,domain:usize)->Result<(),Error> {
        let req=request(layout,domain)?;
        let index=self.pages[..self.used].partition_point(|page|page.base<=pointer).checked_sub(1).ok_or(Error::Range)?;
        let page=&mut self.pages[index];
        let slot=page.slot(pointer)?;
        if page.cache!=req.cache {return Err(Error::Layout);}
        if !page.is_live(slot) {return Err(Error::DoubleFree);}
        if page.free!=0 {page.validate_free(page.head)?;} else if page.head!=0 {return Err(Error::Corrupt);}
        // Volatile erasure cannot be optimized away as a dead store.
        for i in (0..page.stride).step_by(core::mem::size_of::<usize>()) {((pointer+i) as *mut usize).write_volatile(0);}
        (pointer as *mut usize).write_volatile(page.head^self.secret^pointer);
        page.head=pointer;page.free+=1;page.live[slot/64]&=!(1<<(slot%64));
        self.live_bytes-=page.stride;self.allocations-=1;
        Ok(())
    }
}
