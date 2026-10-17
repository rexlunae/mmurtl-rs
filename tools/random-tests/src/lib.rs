#![allow(dead_code)]
#[path = "../../../src/memory/elf.rs"] mod elf;
#[path = "../../../src/memory/user_layout.rs"] mod user_layout;
#[cfg(test)] mod elf_tests;
mod scheduler { pub const MAX_CPUS: usize = 2; }
mod arch {
    pub fn without_interrupts<R>(f: impl FnOnce() -> R) -> R { f() }
    pub static CPU: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
    pub fn cpu_index() -> usize { CPU.load(core::sync::atomic::Ordering::Relaxed) }
    pub mod entropy { pub fn counter() -> u64 { 0 } pub fn collect() {} }
}
mod serial { pub fn write_str(_: &str) {} pub fn write_line(_: &str) {} pub fn write_dec(_: u64) {} pub fn write_hex(_: u64) {} }
#[path = "../../../src/random/mod.rs"]
mod random;
#[path = "../../../src/kptr.rs"] mod kptr;
mod memory { pub mod user { pub const USER_BASE: u64 = 0x640000000000; pub const USER_END: u64 = USER_BASE + (1 << 30); } }
#[test]
fn crypto_and_entropy_policy() {
    use chacha20::{ChaCha20, cipher::{KeyIvInit, StreamCipher}};
    // RFC 8439 all-zero ChaCha20 key/nonce block, counter zero.
    let mut block = [0u8; 64];
    ChaCha20::new((&[0; 32]).into(), (&[0; 12]).into()).apply_keystream(&mut block);
    assert_eq!(hex(&block), "76b8e0ada0f13d90405d6ae55386bd28bdd219b8a08ded1aa836efcc8b770dc7da41597c5157488d7724e03fb8d84a376a43b8f41518a11cc387b669b2ee6586");
    // RFC 7693 BLAKE2s-256("abc").
    assert_eq!(hex(&random::crypto::hash(&[b"abc"])), "508c5e8c327c14e2e1a72ba34eeb452f37458b209ed63a294d999b4c86675982");
    let mut g = random::crypto::Generator::new([0; 32]);
    let mut output = [0; 32]; g.fill(&mut output);
    assert_eq!(output, block[32..]); // secret rekey half is not output
    let first = output; g.fill(&mut output); assert_ne!(first, output);
    assert!(!kptr::RAW_ADDRESSES, "normal and emergency address diagnostics must be restricted");
    assert!(!random::ready());
    assert_eq!(format!("{}", kptr::KPtr(0xffff800012345678)), "(ptr)");
    let mut denied = [0x55; 32]; assert!(random::fill(&mut denied).is_err()); assert_eq!(denied, [0x55; 32]);
    random::init(); assert!(!random::ready()); // public timing gets no credit
    let mut bad = [0xff; 32]; random::seed("bad", &mut bad, 256); assert!(!random::ready()); assert_eq!(bad, [0; 32]);
    let repeated: Vec<_> = [7u64.to_le_bytes(); 4].concat(); assert!(!random::healthy(&repeated));
    let mut good = core::array::from_fn::<_, 32, _>(|i| i as u8 + 1);
    random::seed("test", &mut good, 256); assert!(random::ready()); assert_eq!(good, [0; 32]);
    kptr::init();
    let pointer = 0xffff800012345678;
    let label = format!("{}", kptr::KPtr(pointer));
    assert!(label.starts_with("ptr:") && !label.contains("ffff8000"));
    assert_eq!(label, format!("{}", kptr::KPtr(pointer)));
    assert_ne!(label, format!("{}", kptr::KPtr(pointer + 4096)));
    assert_eq!(label, format!("{}", kptr::UserAddress(pointer)));
    assert_eq!(format!("{}", kptr::UserAddress(memory::user::USER_BASE)), "0x640000000000");
    assert_ne!(random::crypto::keyed_hash(&[1; 32], b"pointer"), random::crypto::keyed_hash(&[2; 32], b"pointer"));
    random::fill(&mut output).unwrap(); let first = output;
    random::fill(&mut output).unwrap(); assert_ne!(first, output);
    let mut new_seed = core::array::from_fn::<_, 32, _>(|i| i as u8 + 33);
    random::seed("test reseed", &mut new_seed, 0); // mixed input still changes epoch/state
    random::fill(&mut output).unwrap(); assert_ne!(first, output);
    assert_ne!(random::early_u64(), random::early_u64());
    let cpu0 = random::u64().unwrap();
    arch::CPU.store(1, core::sync::atomic::Ordering::Relaxed);
    let cpu1 = random::u64().unwrap(); assert_ne!(cpu0, cpu1);
    arch::CPU.store(0, core::sync::atomic::Ordering::Relaxed);
    assert!(random::u64().is_ok());
    random::crypto::erase(&mut output); assert_eq!(output, [0; 32]);
}
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|b| format!("{b:02x}")).collect() }
