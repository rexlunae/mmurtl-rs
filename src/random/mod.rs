//! Kernel entropy pool. Timing and public machine data are mixed with ZERO
//! entropy credit. Hardware/firmware/host RNG credits are conditional on those
//! sources being trustworthy; health checks cannot certify their entropy.
//! No NMI handler may call these APIs. CPU-local state is protected by IRQ
//! masking, with the global pool touched only on initialization/reseeding.
pub mod crypto;
use core::{cell::UnsafeCell, sync::atomic::{AtomicBool, AtomicU64, Ordering}};
use spin::Mutex;
struct Pool { key: [u8; 32], serial: u64, bits: u16 }
static POOL: Mutex<Pool> = Mutex::new(Pool { key: [0; 32], serial: 0, bits: 0 });
static READY: AtomicBool = AtomicBool::new(false);
static EPOCH: AtomicU64 = AtomicU64::new(1);
struct Local { generator: Option<crypto::Generator>, epoch: u64 }
struct CpuLocal(UnsafeCell<Local>);
// Each slot is accessed only by its CPU, with IRQs masked. NMIs never use RNG.
unsafe impl Sync for CpuLocal {}
static LOCAL: [CpuLocal; crate::scheduler::MAX_CPUS] = [const { CpuLocal(UnsafeCell::new(Local { generator: None, epoch: 0 })) }; crate::scheduler::MAX_CPUS];
static TIMING: [AtomicU64; crate::scheduler::MAX_CPUS] = [const { AtomicU64::new(0) }; crate::scheduler::MAX_CPUS];

pub fn mix(source: &[u8], bytes: &[u8], credited_bits: u16) {
    let credited_bits=if cfg!(feature="test-weak-entropy") {0} else {credited_bits};
    crate::arch::without_interrupts(|| {
        let mut p = POOL.lock();
        let key = crypto::hash(&[b"MMURTL entropy v1", &p.key, &(source.len() as u64).to_le_bytes(), source, bytes]);
        crypto::erase(&mut p.key); p.key = key;
        p.bits = p.bits.max(credited_bits.min(256));
        READY.store(p.bits >= 128, Ordering::Release);
        EPOCH.fetch_add(1, Ordering::Release);
    });
}
/// Conservative source credit: no constants, repeated words, or stuck outputs.
/// This is a failure detector, not an entropy estimator.
pub fn healthy(bytes: &[u8]) -> bool {
    if bytes.len() < 8 || bytes.len() % 8 != 0 { return false; }
    let mut previous = None;
    for word in bytes.chunks_exact(8) {
        let value = u64::from_le_bytes(word.try_into().unwrap());
        if value == 0 || value == u64::MAX || previous == Some(value) { return false; }
        previous = Some(value);
    }
    true
}
pub fn seed(source: &'static str, bytes: &mut [u8], bits: u16) {
    let credit = seed_quiet(source, bytes, bits);
    crate::serial::write_str("[RNG] source "); crate::serial::write_str(source);
    crate::serial::write_str(": credited "); crate::serial::write_dec(credit as u64);
    crate::serial::write_line(" bits (conditional source trust)");
}
/// Allocation-free seeding before a console/virtual map exists.
pub fn seed_quiet(source: &str, bytes: &mut [u8], bits: u16) -> u16 {
    let credit = if healthy(bytes) { bits.min(bytes.len().saturating_mul(8).min(256) as u16) } else { 0 };
    mix(source.as_bytes(), bytes, credit);
    crypto::erase(bytes);
    credit
}
fn derive(cpu: u64) -> [u8; 32] {
    let mut p = POOL.lock();
    p.serial = p.serial.checked_add(1).expect("RNG derivation counter exhausted");
    let mut sample = [0u8; crate::scheduler::MAX_CPUS * 8];
    for (i, t) in TIMING.iter().enumerate() { sample[i * 8..i * 8 + 8].copy_from_slice(&t.load(Ordering::Relaxed).to_le_bytes()); }
    let key = crypto::hash(&[b"MMURTL CPU key v1", &p.key, &p.serial.to_le_bytes(), &cpu.to_le_bytes(), &sample]);
    let next = crypto::hash(&[b"MMURTL pool ratchet v1", &p.key, &key]);
    crypto::erase(&mut p.key); p.key = next;
    crypto::erase(&mut sample);
    key
}
pub fn ready() -> bool { READY.load(Ordering::Acquire) }
/// Fails closed when only uncredited timing/public inputs are available.
pub fn fill(out: &mut [u8]) -> Result<(), ()> {
    crate::arch::without_interrupts(|| {
        if !ready() { return Err(()); }
        let cpu = crate::arch::cpu_index();
        let state = unsafe { &mut *LOCAL[cpu].0.get() };
        let epoch = EPOCH.load(Ordering::Acquire);
        if state.generator.is_none() || state.epoch != epoch {
            state.generator = Some(crypto::Generator::new(derive(cpu as u64)));
            state.epoch = epoch;
        }
        state.generator.as_mut().unwrap().fill(out);
        Ok(())
    })
}
pub fn u64() -> Result<u64, ()> { let mut b = [0; 8]; fill(&mut b)?; Ok(u64::from_le_bytes(b)) }
/// Always available without a heap. May have NO credited entropy: callers
/// must consult ready() before claiming ASLR/key security. Never use for keys.
pub fn early_u64() -> u64 {
    crate::arch::without_interrupts(|| {
        let mut g = crypto::Generator::new(derive(u64::MAX));
        let mut b = [0; 8]; g.fill(&mut b); u64::from_le_bytes(b)
    })
}
/// Lock-free timing supplement from normal IRQs; no entropy credit.
pub fn interrupt_sample() {
    let cpu = crate::arch::cpu_index();
    let t = crate::arch::entropy::counter();
    TIMING[cpu].fetch_xor(t.rotate_left((t & 63) as u32), Ordering::Relaxed);
}
pub fn init() {
    crate::arch::entropy::collect();
    for _ in 0..256 {
        let before = crate::arch::entropy::counter();
        core::hint::black_box(before.wrapping_mul(0x9e3779b97f4a7c15));
        let delta = crate::arch::entropy::counter().wrapping_sub(before);
        mix(b"counter jitter, uncredited", &delta.to_le_bytes(), 0);
    }
}
pub fn report() {
    let bits = crate::arch::without_interrupts(|| POOL.lock().bits);
    crate::serial::write_str("[RNG] credited seed bits: "); crate::serial::write_dec(bits as u64);
    crate::serial::write_line(if bits >= 128 { "; CSPRNG ready" } else { "; weak seed, security randomness unavailable" });
    // Consume and discard a sample. Only its one-way hash is logged.
    let mut sample = [0; 32];
    if fill(&mut sample).is_ok() {
        let digest = crypto::hash(&[b"MMURTL boot RNG fingerprint", &sample]);
        crypto::erase(&mut sample);
        crate::serial::write_str("[RNG] discarded-output fingerprint: ");
        crate::serial::write_hex(u64::from_le_bytes(digest[..8].try_into().unwrap()));
        crate::serial::write_str("\n");
    }
}
