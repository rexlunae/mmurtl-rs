//! Console input — a bounded character/event queue, plus the amd64 PS/2
//! keyboard driver (scancode set 1 → characters) that feeds it.
//!
//! On amd64 the IRQ1 handler feeds raw scancodes in; this module tracks
//! modifier state (shift) and translates make codes to ASCII. On arm64 the
//! PL011 UART's RX interrupt pushes characters directly. Either way, tasks
//! consume them with `pop_input`.

use core::sync::atomic::{AtomicU8, Ordering};

const QUEUE_SIZE: usize = 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum InputEvent {
    Byte(u8),
    Lost,
}

struct InputQueue {
    bytes: [u8; QUEUE_SIZE],
    head: usize,
    tail: usize,
    len: usize,
    lost: bool,
    discarding: bool,
}
impl InputQueue {
    const fn new() -> Self {
        Self {
            bytes: [0; QUEUE_SIZE],
            head: 0,
            tail: 0,
            len: 0,
            lost: false,
            discarding: false,
        }
    }
    fn invalidate(&mut self) {
        // Forget *all* buffered lines. Older newlines can never clear
        // recovery for a later truncated line.
        self.head = 0;
        self.tail = 0;
        self.len = 0;
        self.lost = true;
        self.discarding = true;
    }
    fn push(&mut self, byte: u8) {
        if self.discarding {
            if matches!(byte, b'\n' | b'\r') {
                self.discarding = false;
            }
            return;
        }
        if self.len == QUEUE_SIZE {
            self.invalidate();
            if matches!(byte, b'\n' | b'\r') {
                self.discarding = false;
            }
            return;
        }
        self.bytes[self.head] = byte;
        self.head = (self.head + 1) % QUEUE_SIZE;
        self.len += 1;
    }
    fn pop(&mut self) -> Option<InputEvent> {
        if self.lost {
            self.lost = false;
            return Some(InputEvent::Lost);
        }
        if self.len == 0 {
            return None;
        }
        let byte = self.bytes[self.tail];
        self.tail = (self.tail + 1) % QUEUE_SIZE;
        self.len -= 1;
        Some(InputEvent::Byte(byte))
    }
}
// Keyboard and UART IRQs plus the UART poll fallback can all produce.
// Disable local interrupts while locking, so an IRQ never waits on a
// preempted task on its own CPU. Each critical section is constant-time.
static INPUT: spin::Mutex<InputQueue> = spin::Mutex::new(InputQueue::new());

pub fn push_char(c: u8) {
    crate::arch::without_interrupts(|| INPUT.lock().push(c));
}

/// Hardware RX errors must flush the UART first; then call this before
/// accepting new bytes. Recovery ends at a new input line boundary.
pub fn input_lost() {
    crate::arch::without_interrupts(|| INPUT.lock().invalidate());
}

pub fn pop_input() -> Option<InputEvent> {
    crate::arch::without_interrupts(|| INPUT.lock().pop())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn overflow_cannot_execute_a_later_truncated_command() {
        let mut queue = InputQueue::new();
        for _ in 0..QUEUE_SIZE - 10 {
            queue.push(b'\n');
        }
        for &c in b"DEL backup.txt" {
            queue.push(c);
        }
        assert_eq!(queue.pop(), Some(InputEvent::Lost));
        assert_eq!(queue.pop(), None);
        queue.push(b'\n'); // a later Enter must not execute DEL backup
        assert_eq!(queue.pop(), None);
        for &c in b"ECHO safe\n" {
            queue.push(c);
        }
        for &expected in b"ECHO safe\n" {
            assert_eq!(queue.pop(), Some(InputEvent::Byte(expected)));
        }
        assert_eq!(queue.pop(), None);
    }
    #[test]
    fn loss_event_precedes_recovered_bytes() {
        let mut queue = InputQueue::new();
        queue.push(b'x');
        queue.invalidate();
        for &c in b"truncated\rECHO safe\n" {
            queue.push(c);
        }
        assert_eq!(queue.pop(), Some(InputEvent::Lost));
        assert_eq!(queue.pop(), Some(InputEvent::Byte(b'E')));
    }
}

// ========================================================================
// Scancode set 1 translation (amd64 PS/2; arm64 feeds characters from
// the UART straight into the queue)
// ========================================================================

/// Modifier state bits
#[cfg(target_arch = "x86_64")]
const MOD_SHIFT: u8 = 1;
#[cfg(target_arch = "x86_64")]
static MODIFIERS: AtomicU8 = AtomicU8::new(0);

/// Scancode set 1, unshifted (index = make code, 0 = no mapping)
#[cfg(target_arch = "x86_64")]
#[rustfmt::skip]
static MAP_LOWER: [u8; 0x40] = [
    0, 0x1B, b'1', b'2', b'3', b'4', b'5', b'6',       // 00-07 (esc, 1-6)
    b'7', b'8', b'9', b'0', b'-', b'=', 0x08, b'\t',   // 08-0F (bksp, tab)
    b'q', b'w', b'e', b'r', b't', b'y', b'u', b'i',    // 10-17
    b'o', b'p', b'[', b']', b'\n', 0, b'a', b's',      // 18-1F (enter, lctrl)
    b'd', b'f', b'g', b'h', b'j', b'k', b'l', b';',    // 20-27
    b'\'', b'`', 0, b'\\', b'z', b'x', b'c', b'v',     // 28-2F (lshift)
    b'b', b'n', b'm', b',', b'.', b'/', 0, b'*',       // 30-37 (rshift)
    0, b' ', 0, 0, 0, 0, 0, 0,                          // 38-3F (alt, space, caps, F1-F5)
];

#[cfg(target_arch = "x86_64")]
#[rustfmt::skip]
static MAP_UPPER: [u8; 0x40] = [
    0, 0x1B, b'!', b'@', b'#', b'$', b'%', b'^',
    b'&', b'*', b'(', b')', b'_', b'+', 0x08, b'\t',
    b'Q', b'W', b'E', b'R', b'T', b'Y', b'U', b'I',
    b'O', b'P', b'{', b'}', b'\n', 0, b'A', b'S',
    b'D', b'F', b'G', b'H', b'J', b'K', b'L', b':',
    b'"', b'~', 0, b'|', b'Z', b'X', b'C', b'V',
    b'B', b'N', b'M', b'<', b'>', b'?', 0, b'*',
    0, b' ', 0, 0, 0, 0, 0, 0,
];

/// Feed one raw scancode from the IRQ1 handler
#[cfg(target_arch = "x86_64")]
pub fn handle_scancode(scancode: u8) {
    // Extended-key prefix (arrows, right ctrl, ...) — next byte is E0-coded;
    // we don't map those yet, and the prefix itself needs no state.
    if scancode == 0xE0 {
        return;
    }

    let released = scancode & 0x80 != 0;
    let code = scancode & 0x7F;

    // Shift keys (left 0x2A, right 0x36)
    if code == 0x2A || code == 0x36 {
        if released {
            MODIFIERS.fetch_and(!MOD_SHIFT, Ordering::Relaxed);
        } else {
            MODIFIERS.fetch_or(MOD_SHIFT, Ordering::Relaxed);
        }
        return;
    }

    if released || code as usize >= MAP_LOWER.len() {
        return;
    }

    let shifted = MODIFIERS.load(Ordering::Relaxed) & MOD_SHIFT != 0;
    let c = if shifted {
        MAP_UPPER[code as usize]
    } else {
        MAP_LOWER[code as usize]
    };
    if c != 0 {
        push_char(c);
    }
}
