//! Address diagnostics use boot-secret identifiers; raw output is opt-in.
use core::fmt;
pub const RAW_ADDRESSES: bool = cfg!(feature = "debug-addresses");
static KEY: spin::Once<[u8; 32]> = spin::Once::new();
pub fn init() {
    if !crate::random::ready() { return; }
    KEY.call_once(|| { let mut key = [0; 32]; crate::random::fill(&mut key).expect("ready RNG"); key });
}
pub struct KPtr(pub u64);
impl fmt::Display for KPtr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if RAW_ADDRESSES { return write!(f, "0x{:x}", self.0); }
        let Some(key) = KEY.get() else { return f.write_str("(ptr)"); };
        let mut hash = crate::random::crypto::keyed_hash(key, &self.0.to_le_bytes());
        let id = u64::from_le_bytes(hash[..8].try_into().unwrap());
        crate::random::crypto::erase(&mut hash);
        write!(f, "ptr:{:016x}", id)
    }
}
/// Only addresses inside the task window are safe to print verbatim.
pub struct UserAddress(pub u64);
impl fmt::Display for UserAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if (crate::memory::user::USER_BASE..crate::memory::user::USER_END).contains(&self.0) {
            write!(f, "0x{:x}", self.0)
        } else { KPtr(self.0).fmt(f) }
    }
}
pub fn write(pointer: u64) {
    use fmt::Write;
    let mut line: heapless::String<32> = heapless::String::new();
    let _ = write!(line, "{}", KPtr(pointer));
    crate::serial::write_str(&line);
}

pub fn report(image: u64, physmap: u64) {
    use fmt::Write;
    let mut line: heapless::String<160> = heapless::String::new();
    let _ = write!(line, "[KASLR] image={} physmap={} (independent placements)\n", KPtr(image), KPtr(physmap));
    crate::serial::write_str(&line);
}
