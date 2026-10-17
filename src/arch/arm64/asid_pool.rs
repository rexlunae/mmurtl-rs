//! CPU-independent ASID generation bookkeeping. All mutations are serialized
//! with TTBR0 writes and the architectural broadcast invalidation by the caller.
const WORDS: usize = 1024; // 65,536 possible 16-bit tags
#[derive(Clone, Copy)]
struct Space { root: u64, tag: u16, generation: u64 }
const EMPTY: Space = Space { root: 0, tag: 0, generation: 0 };
pub struct Asids<const SPACES: usize, const CPUS: usize> {
    spaces: [Space; SPACES],
    active: [(u64, u16); CPUS],
    used: [u64; WORDS],
    limit: u32,
    next: u32,
    generation: u64,
}
impl<const S: usize, const C: usize> Asids<S, C> {
    pub const fn new(bits: u8) -> Self {
        assert!(bits > 0 && bits <= 16);
        let mut used = [0; WORDS]; used[0] = 1; // ASID 0 is the empty kernel TTBR0
        Self { spaces: [EMPTY; S], active: [(0, 0); C], used, limit: (1u32 << bits) - 1, next: 1, generation: 1 }
    }
    pub fn configure(&mut self, bits: u8) {
        assert!(self.spaces.iter().all(|s| s.root == 0));
        assert!(self.active.iter().all(|(root, _)| *root == 0));
        *self = Self::new(bits);
    }
    pub fn register(&mut self, root: u64) -> bool {
        assert!(root != 0 && !self.spaces.iter().any(|s| s.root == root));
        if let Some(s) = self.spaces.iter_mut().find(|s| s.root == 0) { s.root = root; true } else { false }
    }
    fn used(&self, tag: u32) -> bool { self.used[tag as usize / 64] & (1 << (tag % 64)) != 0 }
    fn mark(&mut self, tag: u32) { self.used[tag as usize / 64] |= 1 << (tag % 64); }
    fn allocate(&mut self) -> Option<u16> {
        for _ in 0..self.limit {
            let tag = self.next;
            self.next = if tag == self.limit { 1 } else { tag + 1 };
            if !self.used(tag) { self.mark(tag); return Some(tag as u16); }
        }
        None
    }
    fn rollover(&mut self) {
        self.generation = self.generation.checked_add(1).expect("ASID generation exhausted");
        self.used.fill(0); self.mark(0); self.next = 1;
        // Preserve every currently loaded tag. Other CPUs may still execute
        // with it until their next switch; it cannot be reassigned meanwhile.
        for cpu in 0..C {
            let (root, tag) = self.active[cpu];
            if root == 0 { continue; }
            assert!(tag != 0 && tag as u32 <= self.limit);
            self.mark(tag as u32);
            let s = self.spaces.iter_mut().find(|s| s.root == root).expect("active ASID root missing");
            s.tag = tag; s.generation = self.generation;
        }
    }
    /// The caller must broadcast and complete TLBI before loading any returned
    /// tag when `flush_all` is true, even if the tag space is exhausted.
    pub fn activate(&mut self, cpu: usize, root: u64) -> (Option<u16>, bool) {
        if root == 0 { self.active[cpu] = (0, 0); return (Some(0), false); }
        let index = self.spaces.iter().position(|s| s.root == root).expect("unknown user TTBR0 root");
        let mut flush = false;
        if self.spaces[index].generation != self.generation {
            let tag = match self.allocate() {
                Some(tag) => Some(tag),
                None => {
                    self.rollover(); flush = true;
                    if self.spaces[index].generation == self.generation { Some(self.spaces[index].tag) } else { self.allocate() }
                }
            };
            let Some(tag) = tag else { return (None, flush); };
            self.spaces[index].tag = tag; self.spaces[index].generation = self.generation;
        }
        let tag = self.spaces[index].tag;
        self.active[cpu] = (root, tag);
        (Some(tag), flush)
    }
    pub fn tag(&self, root: u64) -> Option<u16> {
        self.spaces.iter().find(|s| s.root == root && s.generation == self.generation).map(|s| s.tag)
    }
    pub fn release(&mut self, root: u64) {
        assert!(!self.active.iter().any(|(r, _)| *r == root), "freeing active address space");
        let s = self.spaces.iter_mut().find(|s| s.root == root).expect("unregistered address space");
        *s = EMPTY; // retired tags are reused only after a generation flush
    }
    pub fn generation(&self) -> u64 { self.generation }
}
