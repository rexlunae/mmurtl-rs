//! Scalar RustCrypto primitives and a fast-key-erasure ChaCha20 generator.
use blake2::{Blake2s256, Digest};
use chacha20::{ChaCha20, cipher::{KeyIvInit, StreamCipher}};

pub fn hash(parts: &[&[u8]]) -> [u8; 32] {
    let mut h = Blake2s256::new();
    for part in parts { h.update(part); }
    h.finalize().into()
}
pub fn erase(bytes: &mut [u8]) {
    for b in bytes { unsafe { core::ptr::write_volatile(b, 0); } }
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
}
pub struct Generator { key: [u8; 32] }
impl Generator {
    pub const fn new(key: [u8; 32]) -> Self { Self { key } }
    pub fn fill(&mut self, out: &mut [u8]) {
        for chunk in out.chunks_mut(32) {
            let mut block = [0u8; 64];
            ChaCha20::new((&self.key).into(), (&[0u8; 12]).into()).apply_keystream(&mut block);
            // The first half is never returned: it replaces the old key.
            erase(&mut self.key);
            self.key.copy_from_slice(&block[..32]);
            chunk.copy_from_slice(&block[32..32 + chunk.len()]);
            erase(&mut block);
        }
    }
}
impl Drop for Generator { fn drop(&mut self) { erase(&mut self.key); } }

/// Keyed BLAKE2s for pointer identifiers, separate from entropy derivation.
pub fn keyed_hash(key: &[u8; 32], pointer: &[u8]) -> [u8; 32] {
    use blake2::{Blake2sMac256, digest::{KeyInit, Mac}};
    let mut h = <Blake2sMac256 as KeyInit>::new_from_slice(key).expect("32-byte key");
    Mac::update(&mut h, b"MMURTL pointer identifier v1");
    Mac::update(&mut h, pointer);
    h.finalize().into_bytes().into()
}
