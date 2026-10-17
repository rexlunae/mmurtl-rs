//! x18 is reserved across all compiled kernel/core/alloc code. Each task
//! owns a separately guarded shadow stack; EL0's x18 is never trusted.
use core::sync::atomic::{AtomicU64,Ordering};
#[repr(align(16))]
struct Early([u8;8192]);
#[no_mangle]
static mut ARM64_EARLY_SCS:Early=Early([0;8192]);
#[no_mangle]
pub static ARM64_USER_SCS:[AtomicU64;crate::scheduler::MAX_CPUS]=[const {AtomicU64::new(0)};crate::scheduler::MAX_CPUS];
pub fn prepare(cpu:usize,base:u64) {ARM64_USER_SCS[cpu].store(base,Ordering::Release);}
#[cfg(feature="test-shadow-overflow")]
pub fn test_overflow()->! {
    extern "C" fn overflow()->! {
        unsafe {core::arch::asm!("and x18, x18, #-65536","add x18, x18, #9, lsl #12","str x30, [x18]","brk #0",options(noreturn));}
    }
    crate::scheduler::create_task(overflow,crate::scheduler::PRIORITY_DEFAULT,"stack_overflow");
    crate::scheduler::start();super::enable_interrupts();loop {super::halt();}
}
