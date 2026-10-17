//! Private user-window placement. Slot IDs track ownership, not addresses.
pub const BASE: u64 = 0x0000_6400_0000_0000;
pub const END: u64 = BASE + (1 << 39);
pub const PAGE: u64 = 4096;
pub const STACK_PAGES: u64 = 4;
pub const IMAGE_LIMIT: u64 = BASE + (1 << 38);
pub const STARTUP_MAGIC: u64 = 0x4d4d_5552_544c_0001;
#[derive(Clone,Copy)]
pub struct Layout { pub bias: u64, pub stack_edge: u64, pub stack_pointer: u64 }
impl Layout {
    pub fn image_bias(self, alignment: u64) -> u64 {
        assert!(alignment>=PAGE && alignment<=65536 && alignment.is_power_of_two());
        (self.bias & !(alignment-1))+alignment
    }
}
pub fn choose(code: u64, stack: u64, sp: u64) -> Layout {
    let bias=BASE+PAGE+(code&0x1ffffff)*PAGE; // 25 page-placement bits
    let stack_edge=IMAGE_LIMIT+(16+(stack&0x1ffffff))*PAGE;
    Layout { bias,stack_edge,stack_pointer:stack_edge-64-(sp&0x3f0) }
}
