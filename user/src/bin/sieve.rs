//! A Rust user program: sieve of Eratosthenes over a stack array, with
//! mutable static data (exercises .data/.bss segments in the ELF loader).
#![no_std]
#![no_main]

use mmurtl::println;

mmurtl::entry!(main);

static mut RUNS: u32 = 7; // .data
static mut SIEVE: [bool; 10_000] = [false; 10_000]; // .bss

fn main() {
    let sieve = unsafe { &mut *core::ptr::addr_of_mut!(SIEVE) };
    let mut count = 0;
    for i in 2..sieve.len() {
        if !sieve[i] {
            count += 1;
            let mut j = i * i;
            while j < sieve.len() {
                sieve[j] = true;
                j += i;
            }
        }
    }
    let runs = unsafe {
        RUNS += 1;
        RUNS
    };
    println!("sieve: {} primes below 10000 (data segment says {})", count, runs);
}
