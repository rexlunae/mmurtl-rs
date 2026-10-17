//! Optional diagnostic sampling. Freed sample VAs are never reused. Free
//! unmapping runs on the reaper, outside allocator/scheduler locks with IRQs
//! enabled; until then the sample is scrubbed but still mapped.
use core::{alloc::Layout,sync::atomic::{AtomicUsize,Ordering}};
const EMPTY:usize=0;const RESERVED:usize=1;const LIVE:usize=2;
const QUEUED:usize=3;const UNMAPPING:usize=4;const SCRUBBING:usize=5;const FREED:usize=6;
struct Sample {
    state:AtomicUsize, pointer:AtomicUsize,base:AtomicUsize,phys:AtomicUsize,
    size:AtomicUsize,align:AtomicUsize,domain:AtomicUsize,
    alloc_task:AtomicUsize,alloc_cpu:AtomicUsize,free_task:AtomicUsize,free_cpu:AtomicUsize,
}
impl Sample {
    const fn new()->Self {Self {state:AtomicUsize::new(0),pointer:AtomicUsize::new(0),base:AtomicUsize::new(0),phys:AtomicUsize::new(0),size:AtomicUsize::new(0),align:AtomicUsize::new(0),domain:AtomicUsize::new(0),alloc_task:AtomicUsize::new(0),alloc_cpu:AtomicUsize::new(0),free_task:AtomicUsize::new(0),free_cpu:AtomicUsize::new(0)}}
}
static SAMPLES:[Sample;128]=[const {Sample::new()};128];
fn context()->(usize,usize) {crate::arch::without_interrupts(|| {let cpu=crate::arch::cpu_index();(cpu,super::stacks::current_task(cpu) as usize)})}
pub fn allocate(layout:Layout,domain:usize,force:bool)->Option<*mut u8> {
    // Sample generic/user-sized allocations. Dedicated task and inbox
    // caches keep their bounded cache footprint and frame accounting.
    if domain!=0||!crate::scheduler::running()||!crate::arch::stacks::interrupts_enabled()||layout.size()>4096||layout.align()>4096 {return None;}
    if !force && crate::random::u64().unwrap_or_else(|_|crate::random::early_u64())&63!=0 {return None;}
    let sample=SAMPLES.iter().find(|sample|sample.state.compare_exchange(EMPTY,RESERVED,Ordering::AcqRel,Ordering::Acquire).is_ok())?;
    let stack=super::stacks::allocate_sample_page();
    let pointer=(stack.virt+stack.size-layout.size().max(1) as u64)&!(layout.align() as u64-1);
    let (cpu,task)=context();
    for (field,value) in [(&sample.pointer,pointer as usize),(&sample.base,stack.virt as usize),(&sample.phys,stack.phys as usize),(&sample.size,layout.size()),(&sample.align,layout.align()),(&sample.domain,domain),(&sample.alloc_cpu,cpu),(&sample.alloc_task,task)] {field.store(value,Ordering::Relaxed);}
    sample.state.store(LIVE,Ordering::Release);
    Some(pointer as *mut u8)
}
pub unsafe fn deallocate(pointer:*mut u8,layout:Layout,domain:usize)->bool {
    for sample in &SAMPLES {
        let state=sample.state.load(Ordering::Acquire);
        if state< LIVE||sample.pointer.load(Ordering::Relaxed)!=pointer as usize {continue;}
        if sample.size.load(Ordering::Relaxed)!=layout.size()||sample.align.load(Ordering::Relaxed)!=layout.align()||sample.domain.load(Ordering::Relaxed)!=domain
            ||sample.state.compare_exchange(LIVE,SCRUBBING,Ordering::AcqRel,Ordering::Acquire).is_err() {super::heap::integrity_failure();}
        for i in 0..layout.size() {pointer.add(i).write_volatile(0);}
        let (cpu,task)=context();sample.free_cpu.store(cpu,Ordering::Relaxed);sample.free_task.store(task,Ordering::Relaxed);
        sample.state.store(QUEUED,Ordering::Release);return true;
    }
    false
}
pub fn reap() {
    for sample in &SAMPLES {
        if sample.state.compare_exchange(QUEUED,UNMAPPING,Ordering::AcqRel,Ordering::Acquire).is_ok() {
            super::stacks::release_sample_page(sample.base.load(Ordering::Relaxed) as u64,sample.phys.load(Ordering::Relaxed) as u64);
            sample.state.store(FREED,Ordering::Release);
        }
    }
}
/// Fault paths cannot acquire locks: the interrupted code may own them.
pub fn fault(address:u64,pc:u64) {
    for sample in &SAMPLES {
        let state=sample.state.load(Ordering::Acquire);
        if state< LIVE||state==SCRUBBING {continue;}
        let base=sample.base.load(Ordering::Relaxed) as u64;
        if !(base-4096..base+8192).contains(&address) {continue;}
        let freed=matches!(state,QUEUED|UNMAPPING|FREED);
        fn write(s:&[u8]) {for b in s {crate::arch::stacks::emergency_byte(*b);}}
        fn number(mut n:usize) {
            let mut digits=[0u8;20];let mut index=digits.len();
            loop {index-=1;digits[index]=b'0'+(n%10) as u8;n/=10;if n==0 {break;}}
            write(&digits[index..]);
        }
        write(if freed {b"\n[KFENCE] use-after-free; alloc T"} else {b"\n[KFENCE] guard overflow; alloc T"});
        number(sample.alloc_task.load(Ordering::Relaxed));write(b"/CPU");number(sample.alloc_cpu.load(Ordering::Relaxed));
        write(b"; free T");number(sample.free_task.load(Ordering::Relaxed));write(b"/CPU");number(sample.free_cpu.load(Ordering::Relaxed));
        write(b"; cache ");number(sample.domain.load(Ordering::Relaxed));write(b"; size ");number(sample.size.load(Ordering::Relaxed));write(b"\n");
        crate::cfi::fatal(if freed {b"KFENCE use-after-free"} else {b"KFENCE guard overflow"},pc,Some(address));
    }
}
#[cfg(feature="test-kfence")]
pub fn test()->! {
    extern "C" fn probe()->! {
        let layout=Layout::from_size_align(64,16).unwrap();
        let pointer=allocate(layout,0,true).expect("forced KFENCE sample");
        unsafe {pointer.write_volatile(0xa5);assert!(deallocate(pointer,layout,0));}
        while !SAMPLES.iter().any(|s|s.state.load(Ordering::Acquire)==FREED&&s.pointer.load(Ordering::Relaxed)==pointer as usize) {crate::scheduler::sleep_ms(10);}
        crate::serial::write_line("[KFENCE TEST] touching unmapped object after recorded allocation/free");
        unsafe {pointer.write_volatile(0x55);}
        panic!("KFENCE use-after-free escaped");
    }
    crate::scheduler::start_reaper();
    crate::scheduler::create_task(probe,crate::scheduler::PRIORITY_DEFAULT,"kfence_probe");
    crate::scheduler::start();crate::arch::enable_interrupts();loop {crate::arch::halt();}
}
