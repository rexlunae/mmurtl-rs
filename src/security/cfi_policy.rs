//! CFI feature fields and owned architectural control bits.
pub const CR4_CET:u64=1<<23;
pub const SUPERVISOR_ENDBR:u64=1<<2;
pub const SCTLR_BT1:u64=1<<36;
pub const PTE_GP:u64=1<<50;
pub fn arm_bti(pfr1:u64)->bool {pfr1&15==1}
