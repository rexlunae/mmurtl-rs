//! CPU-owned descriptor metadata. Never reconstruct ownership from DMA memory.
use alloc::{boxed::Box, vec};
const END: u16 = u16::MAX;
pub struct Ownership {
    next: Box<[u16]>,
    pending: Box<[u16]>,
    free_head: u16,
    free: u16,
    broken: bool,
}
impl Ownership {
    pub fn new(size: u16) -> Self {
        assert!(size != 0);
        let mut next = vec![END; size as usize].into_boxed_slice();
        for i in 0..size - 1 { next[i as usize] = i + 1; }
        Self { next, pending: vec![0; size as usize].into_boxed_slice(), free_head: 0, free: size, broken: false }
    }
    pub fn broken(&self) -> bool { self.broken }
    pub fn poison(&mut self) { self.broken = true; }
    pub fn next(&self, id: u16) -> u16 { self.next[id as usize] }
    pub fn reserve(&mut self, count: usize) -> Option<u16> {
        if self.broken || count == 0 || count > self.free as usize { return None; }
        let head = self.free_head;
        let mut tail = head;
        for _ in 1..count { tail = self.next(tail); }
        self.free_head = self.next(tail);
        self.next[tail as usize] = END;
        self.pending[head as usize] = count as u16;
        self.free -= count as u16;
        Some(head)
    }
    /// A malformed completion permanently stops this queue until device reset.
    pub fn complete(&mut self, id: u32, count: u16, expected: Option<u16>) -> Option<u16> {
        if self.broken { return None; }
        if id as usize >= self.pending.len() || count == 0 || self.pending[id as usize] != count
            || expected.is_some_and(|head| id != head as u32) {
            self.poison(); return None;
        }
        let head = id as u16;
        let mut tail = head;
        for _ in 1..count { tail = self.next(tail); }
        self.next[tail as usize] = self.free_head;
        self.free_head = head;
        self.pending[head as usize] = 0;
        self.free += count;
        Some(head)
    }
}
