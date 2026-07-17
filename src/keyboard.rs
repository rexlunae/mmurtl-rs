//! PS/2 keyboard driver — scancode set 1 → characters.
//!
//! The IRQ1 handler feeds raw scancodes in; this module tracks modifier
//! state (shift), translates make codes to ASCII, and queues characters
//! in a small lock-free ring buffer for tasks to consume.

use core::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

// ========================================================================
// Character ring buffer (single producer: IRQ handler on the BSP;
// consumers pop with a CAS loop)
// ========================================================================

const QUEUE_SIZE: usize = 64; // power of two

static QUEUE: [AtomicU8; QUEUE_SIZE] = {
    const ZERO: AtomicU8 = AtomicU8::new(0);
    [ZERO; QUEUE_SIZE]
};
static HEAD: AtomicUsize = AtomicUsize::new(0); // next write
static TAIL: AtomicUsize = AtomicUsize::new(0); // next read

fn push_char(c: u8) {
    let head = HEAD.load(Ordering::Relaxed);
    let tail = TAIL.load(Ordering::Acquire);
    if head.wrapping_sub(tail) >= QUEUE_SIZE {
        return; // full — drop
    }
    QUEUE[head % QUEUE_SIZE].store(c, Ordering::Relaxed);
    HEAD.store(head.wrapping_add(1), Ordering::Release);
}

/// Pop one character from the keyboard queue (any task, any CPU)
pub fn pop_char() -> Option<u8> {
    loop {
        let tail = TAIL.load(Ordering::Relaxed);
        let head = HEAD.load(Ordering::Acquire);
        if tail == head {
            return None;
        }
        let c = QUEUE[tail % QUEUE_SIZE].load(Ordering::Relaxed);
        if TAIL
            .compare_exchange(tail, tail.wrapping_add(1), Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
        {
            return Some(c);
        }
    }
}

// ========================================================================
// Scancode set 1 translation
// ========================================================================

/// Modifier state bits
const MOD_SHIFT: u8 = 1;
static MODIFIERS: AtomicU8 = AtomicU8::new(0);

/// Scancode set 1, unshifted (index = make code, 0 = no mapping)
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
