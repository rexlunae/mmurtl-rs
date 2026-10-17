//! Reclaiming size-class slabs with immutable cache ownership. Metadata is
//! outside client allocations; free-list links are secret/address encoded.
use core::alloc::{AllocError, Allocator, GlobalAlloc, Layout};
use core::ptr::NonNull;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::memory::frame_allocator;

/// Global frame allocator pointer — set once during memory init
static mut FRAME_ALLOC_PTR: *mut frame_allocator::FrameAllocator = core::ptr::null_mut();

/// Serializes every use of the frame allocator across CPUs. Always taken
/// with interrupts disabled. Closures run under it must not allocate from
/// the kernel heap (heap growth takes this lock too).
static FRAME_LOCK: spin::Mutex<()> = spin::Mutex::new(());

/// Frames the heap has taken from the frame allocator by growing (so
/// frame accounting can tell heap growth apart from leaks)
static GROWN_FRAMES: AtomicUsize = AtomicUsize::new(0);

/// Frames consumed by heap growth since boot
pub fn grown_frames() -> usize {
    GROWN_FRAMES.load(Ordering::Relaxed)
}

/// Free frames in the frame allocator
pub fn free_frames() -> usize {
    with_frame_allocator(|fa| fa.free_count()).unwrap_or(0)
}

/// Set the global frame allocator reference (called during memory init)
pub unsafe fn set_frame_allocator(fa: *mut frame_allocator::FrameAllocator) {
    FRAME_ALLOC_PTR = fa;
}

/// Run a closure with mutable access to the global frame allocator.
///
/// Returns None if memory management is not yet initialized. Safe to call
/// from any CPU: access is serialized by FRAME_LOCK (interrupts disabled
/// while held). The closure must not allocate from the kernel heap.
pub fn with_frame_allocator<R>(
    f: impl FnOnce(&mut frame_allocator::FrameAllocator) -> R,
) -> Option<R> {
    crate::arch::without_interrupts(|| {
        let _guard = FRAME_LOCK.lock();
        unsafe {
            let ptr = FRAME_ALLOC_PTR;
            if ptr.is_null() {
                None
            } else {
                Some(f(&mut *ptr))
            }
        }
    })
}


struct HeapState {
    slabs: super::slab::Slabs,
    next: usize,
    end: usize,
}
impl HeapState {
    const fn new() -> Self {Self {slabs:super::slab::Slabs::new(),next:0,end:0}}
}
pub struct KernelAllocator {
    ready: AtomicBool,
    state: spin::Mutex<HeapState>,
}
impl KernelAllocator {
    pub const fn new()->Self {Self {ready:AtomicBool::new(false),state:spin::Mutex::new(HeapState::new())}}
    fn initialize(&self,fa:&mut frame_allocator::FrameAllocator,secret:usize) {
        let (start,end)=crate::arch::heap_init(fa);
        let mut state=self.state.lock();state.next=start as usize;state.end=end as usize;state.slabs.seed(secret);
        self.ready.store(true,Ordering::Release);
        drop(state);
        crate::serial::write_str("[HEAP] Slab allocator at ");crate::kptr::write(start);
        crate::serial::write_str(" (");crate::serial::write_dec((end-start)/1024);crate::serial::write_line(" KiB initial backing)");
        crate::serial::write_line("[HEAP] reclaiming slabs; immutable domain/size caches; randomized encoded free lists; scrub on free");
    }
    fn allocate_domain(&self,layout:Layout,domain:usize)->*mut u8 {
        if !self.ready.load(Ordering::Acquire) {return core::ptr::null_mut();}
        #[cfg(feature="kfence")]
        if let Some(pointer)=super::fence::allocate(layout,domain,false) {return pointer;}
        crate::arch::without_interrupts(|| {
            let mut state=self.state.lock();
            match unsafe {state.slabs.allocate(layout,domain)} {
                Ok(Some(pointer))=>return pointer as *mut u8,
                Ok(None)=>{},
                Err(super::slab::Error::Layout)=>return core::ptr::null_mut(),
                Err(_)=>integrity_failure(),
            }
            if !state.slabs.has_metadata() {return core::ptr::null_mut();}
            let req=super::slab::request(layout,domain).unwrap();
            let Some(base)=state.next.checked_add(req.align-1).map(|v|v&!(req.align-1)) else {return core::ptr::null_mut();};
            let Some(end)=base.checked_add(req.span).filter(|v|*v<=crate::arch::memory::heap_limit() as usize) else {return core::ptr::null_mut();};
            if end>state.end {
                let needed=(end-state.end) as u64;
                let Some(grown)=with_frame_allocator(|fa|crate::arch::heap_extend(state.end as u64,needed,fa)).flatten() else {return core::ptr::null_mut();};
                state.end+=grown as usize;
                GROWN_FRAMES.fetch_add((grown/4096) as usize,Ordering::Relaxed);
            }
            state.next=end;
            if unsafe {state.slabs.add(base,req,||crate::random::u64().unwrap_or_else(|_|crate::random::early_u64()) as usize)}.is_err() {integrity_failure();}
            match unsafe {state.slabs.allocate(layout,domain)} {
                Ok(Some(pointer))=>pointer as *mut u8,
                _=>integrity_failure(),
            }
        })
    }
    unsafe fn deallocate_domain(&self,pointer:*mut u8,layout:Layout,domain:usize) {
        #[cfg(feature="kfence")]
        if super::fence::deallocate(pointer,layout,domain) {return;}
        crate::arch::without_interrupts(|| {
            let mut state=self.state.lock();
            if state.slabs.deallocate(pointer as usize,layout,domain).is_err() {integrity_failure();}
        });
    }
}
pub(super) fn integrity_failure()->! {crate::cfi::fatal(b"heap integrity",0,None)}
unsafe impl GlobalAlloc for KernelAllocator {
    unsafe fn alloc(&self,layout:Layout)->*mut u8 {self.allocate_domain(layout,0)}
    unsafe fn dealloc(&self,pointer:*mut u8,layout:Layout) {self.deallocate_domain(pointer,layout,0);}
}
/// Security-sensitive clients use explicit domains instead of relying on
/// matching allocation sizes to identify object types.
#[derive(Clone,Copy)]
pub struct CacheAllocator<const DOMAIN:usize>;
pub const TASK_ALLOC:CacheAllocator<1>=CacheAllocator;
pub const RQB_ALLOC:CacheAllocator<2>=CacheAllocator;
unsafe impl<const DOMAIN:usize> Allocator for CacheAllocator<DOMAIN> {
    fn allocate(&self,layout:Layout)->Result<NonNull<[u8]>,AllocError> {
        let pointer=NonNull::new(GLOBAL_ALLOC.allocate_domain(layout,DOMAIN)).ok_or(AllocError)?;
        Ok(NonNull::slice_from_raw_parts(pointer,layout.size()))
    }
    unsafe fn deallocate(&self,pointer:NonNull<u8>,layout:Layout) {GLOBAL_ALLOC.deallocate_domain(pointer.as_ptr(),layout,DOMAIN);}
}
#[global_allocator]
static GLOBAL_ALLOC:KernelAllocator=KernelAllocator::new();
/// (live class bytes, permanently cache-owned backing bytes, live objects).
pub fn stats()->(usize,usize,usize) {
    crate::arch::without_interrupts(|| {let state=GLOBAL_ALLOC.state.lock();(state.slabs.live_bytes,state.slabs.reserved_bytes,state.slabs.allocations)})
}
pub fn init_heap() {
    let secret=crate::random::u64().unwrap_or_else(|_|crate::random::early_u64()) as usize;
    if !crate::random::ready() {crate::serial::write_line("[HEAP] WARNING: weak bootstrap entropy; free-list secrecy is not assured");}
    with_frame_allocator(|fa|GLOBAL_ALLOC.initialize(fa,secret)).expect("frame allocator before heap");
    crate::memory::stacks::init();
}
