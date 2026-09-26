//! A Rust user program: talks to the kernel's sysinfo service over RQB
//! IPC, then does a little real work.
#![no_std]
#![no_main]

use mmurtl::{println, Rqb};

mmurtl::entry!(main);

fn main() {
    println!("Rust ELF program running as task {}", mmurtl::tid());

    if let Some(svc) = mmurtl::lookup_service("sysinfo") {
        let mut rqb = Rqb::new(0x0001); // SVC_SYS_INFO
        let status = mmurtl::send_rqb(svc, &mut rqb);
        let reply = core::str::from_utf8(rqb.payload()).unwrap_or("?");
        println!("sysinfo (task {}) replied, status {}: {}", svc, status, reply);
    }

    // Some genuine computation: a Collatz search
    let (mut best, mut best_len) = (1u64, 1u32);
    for n in 1..20_000u64 {
        let (mut x, mut len) = (n, 1u32);
        while x != 1 {
            x = if x % 2 == 0 { x / 2 } else { 3 * x + 1 };
            len += 1;
        }
        if len > best_len {
            (best, best_len) = (n, len);
        }
    }
    println!("longest Collatz chain below 20000 starts at {} ({} steps)", best, best_len);
}
