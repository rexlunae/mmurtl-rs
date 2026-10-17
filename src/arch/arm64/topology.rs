//! MT changes the meaning of Aff0 from core to hardware thread.
pub fn probe() -> crate::topology::policy::Core {
    let mpidr: u64;
    unsafe { core::arch::asm!("mrs {}, mpidr_el1", out(reg) mpidr, options(nomem,nostack)); }
    crate::topology::policy::Core::arm(mpidr)
}
