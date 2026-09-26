//! Architecture layer.
//!
//! Each port implements the same interface, which the portable kernel
//! (scheduler, IPC, memory, virtio, syscalls, userspace) calls as
//! `crate::arch::...`:
//!
//! | area | interface |
//! |---|---|
//! | identity | `NAME`, `SYSCALL_MECHANISM`, `cpus_online()` |
//! | console | `serial::{init, putc, NAME}` |
//! | interrupts | `without_interrupts`, `enable_interrupts`, `halt` |
//! | CPUs | `cpu_index`, `set_cpu_index`, `hw_cpu_id`, `ipi_available`, `send_resched_ipi` |
//! | time | `start_tick(hz, boot_cpu)`, `delay_ms` |
//! | tasks | `TaskContext`, `kernel_context`, `user_context`, `yield_now`, `on_switch_to_user` |
//! | caches | `sync_icache` (after writing code) |
//! | memory | `phys_to_virt`, `heap_init`, `heap_extend`, `map_user_page(space, ..)`, `query_page` (current space), `user_access_begin/end` |
//! | address spaces | `new_address_space`, `free_address_space`, `switch_address_space` |
//! | userspace | `user_programs::{hello, echo, spinner, rogue_read, rogue_priv, rogue_ptr, exit_only}` |
//!
//! The boot path is the architecture's own: it brings up the console,
//! memory, interrupt controller, scheduler, secondary CPUs, and devices,
//! then calls `crate::kernel_run()`.

#[cfg(target_arch = "x86_64")]
mod amd64;
#[cfg(target_arch = "x86_64")]
pub use amd64::*;

#[cfg(target_arch = "aarch64")]
mod arm64;
#[cfg(target_arch = "aarch64")]
pub use arm64::*;
