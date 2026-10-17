pub use super::mmu::{map_stack_page as map, stack_page_mapped as mapped, unmap_stack_page as unmap};
pub fn invalidate() { unsafe { core::arch::asm!("dsb ishst", "tlbi vmalle1is", "dsb ish", "isb"); } }
