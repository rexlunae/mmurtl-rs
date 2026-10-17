//! Uncontended host-only lock acquisition cost; not a kernel latency claim.
#[allow(dead_code)]
#[path = "../../src/arch/amd64/nospec.rs"]
mod nospec;
use std::sync::atomic::{AtomicBool, Ordering};
fn sample(fence: bool) -> f64 {
    let lock = AtomicBool::new(false);
    let start = std::time::Instant::now();
    for _ in 0..100_000 {
        while lock.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {}
        if fence { nospec::barrier_nospec(); }
        std::hint::black_box(&lock);
        lock.store(false, Ordering::Release);
    }
    start.elapsed().as_nanos() as f64 / 100_000.0
}
fn main() {
    let mut plain = [0.0; 7];
    let mut fenced = [0.0; 7];
    for n in 0..7 { plain[n] = sample(false); fenced[n] = sample(true); }
    plain.sort_by(f64::total_cmp); fenced.sort_by(f64::total_cmp);
    println!("host uncontended CAS lock median: {:.1} ns; with CPUID barrier: {:.1} ns ({:.1} ns added)", plain[3], fenced[3], fenced[3] - plain[3]);
}
