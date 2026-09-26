//! USB HID (Human Interface Device) — keyboard and mouse report parsing.
//!
//! USB HID devices send input reports over interrupt endpoints. For a keyboard,
//! the boot protocol report is 8 bytes: modifier keys + reserved + 6 keycodes.
//! For a mouse, boot protocol report is 3+ bytes: buttons + X delta + Y delta.

use core::fmt;

// ========================================================================
// USB HID Boot Protocol — Keyboard
// ========================================================================

/// USB HID keyboard boot protocol report (8 bytes)
#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
pub struct KeyboardReport {
    pub modifiers: u8,      // Modifier keys (bitfield)
    pub reserved: u8,
    pub keys: [u8; 6],     // Up to 6 pressed keys (USB HID usage IDs)
}

/// Modifier key flags
pub const MOD_LCTRL: u8   = 0x01;
pub const MOD_LSHIFT: u8  = 0x02;
pub const MOD_LALT: u8    = 0x04;
pub const MOD_LMETA: u8   = 0x08;  // Windows/Command key
pub const MOD_RCTRL: u8   = 0x10;
pub const MOD_RSHIFT: u8  = 0x20;
pub const MOD_RALT: u8    = 0x40;
pub const MOD_RMETA: u8   = 0x80;

impl KeyboardReport {
    /// Parse from an 8-byte buffer (standard boot protocol report)
    pub fn from_bytes(buf: &[u8; 8]) -> Self {
        Self {
            modifiers: buf[0],
            reserved: buf[1],
            keys: [buf[2], buf[3], buf[4], buf[5], buf[6], buf[7]],
        }
    }

    /// Check if a specific key is pressed
    pub fn has_key(&self, usage: u8) -> bool {
        // First check the key array
        for &k in self.keys.iter() {
            if k == usage {
                return true;
            }
        }
        false
    }

    /// Check if any shift is pressed
    pub fn is_shift_pressed(&self) -> bool {
        self.modifiers & (MOD_LSHIFT | MOD_RSHIFT) != 0
    }

    /// Check if control is pressed
    pub fn is_ctrl_pressed(&self) -> bool {
        self.modifiers & (MOD_LCTRL | MOD_RCTRL) != 0
    }

    /// Check if alt is pressed
    pub fn is_alt_pressed(&self) -> bool {
        self.modifiers & (MOD_LALT | MOD_RALT) != 0
    }

    /// Check if meta (Windows/Command) is pressed
    pub fn is_meta_pressed(&self) -> bool {
        self.modifiers & (MOD_LMETA | MOD_RMETA) != 0
    }

    /// Get the first pressed key's usage ID (0 if none)
    pub fn first_key(&self) -> u8 {
        self.keys[0]
    }
}

impl fmt::Debug for KeyboardReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let has_keys = self.keys.iter().any(|&k| k != 0);
        if !has_keys && self.modifiers == 0 {
            write!(f, "KeyboardReport: [no keys]")
        } else {
            write!(f, "KeyboardReport: mods={:#04x} keys=[", self.modifiers);
            for (i, &k) in self.keys.iter().enumerate() {
                if k != 0 {
                    if i > 0 { write!(f, " ")?; }
                    write!(f, "{:#04x}", k)?;
                }
            }
            write!(f, "]")
        }
    }
}

// ========================================================================
// USB HID Keyboard Usage ID → ASCII Translation
// ========================================================================

/// Map a USB HID keyboard usage ID to an ASCII character, given modifier state
pub fn hid_usage_to_ascii(usage: u8, shift_pressed: bool) -> Option<char> {
    match usage {
        // Letters
        0x04 => Some(if shift_pressed { 'A' } else { 'a' }),
        0x05 => Some(if shift_pressed { 'B' } else { 'b' }),
        0x06 => Some(if shift_pressed { 'C' } else { 'c' }),
        0x07 => Some(if shift_pressed { 'D' } else { 'd' }),
        0x08 => Some(if shift_pressed { 'E' } else { 'e' }),
        0x09 => Some(if shift_pressed { 'F' } else { 'f' }),
        0x0A => Some(if shift_pressed { 'G' } else { 'g' }),
        0x0B => Some(if shift_pressed { 'H' } else { 'h' }),
        0x0C => Some(if shift_pressed { 'I' } else { 'i' }),
        0x0D => Some(if shift_pressed { 'J' } else { 'j' }),
        0x0E => Some(if shift_pressed { 'K' } else { 'k' }),
        0x0F => Some(if shift_pressed { 'L' } else { 'l' }),
        0x10 => Some(if shift_pressed { 'M' } else { 'm' }),
        0x11 => Some(if shift_pressed { 'N' } else { 'n' }),
        0x12 => Some(if shift_pressed { 'O' } else { 'o' }),
        0x13 => Some(if shift_pressed { 'P' } else { 'p' }),
        0x14 => Some(if shift_pressed { 'Q' } else { 'q' }),
        0x15 => Some(if shift_pressed { 'R' } else { 'r' }),
        0x16 => Some(if shift_pressed { 'S' } else { 's' }),
        0x17 => Some(if shift_pressed { 'T' } else { 't' }),
        0x18 => Some(if shift_pressed { 'U' } else { 'u' }),
        0x19 => Some(if shift_pressed { 'V' } else { 'v' }),
        0x1A => Some(if shift_pressed { 'W' } else { 'w' }),
        0x1B => Some(if shift_pressed { 'X' } else { 'x' }),
        0x1C => Some(if shift_pressed { 'Y' } else { 'y' }),
        0x1D => Some(if shift_pressed { 'Z' } else { 'z' }),

        // Numbers
        0x1E => Some(if shift_pressed { '!' } else { '1' }),
        0x1F => Some(if shift_pressed { '@' } else { '2' }),
        0x20 => Some(if shift_pressed { '#' } else { '3' }),
        0x21 => Some(if shift_pressed { '$' } else { '4' }),
        0x22 => Some(if shift_pressed { '%' } else { '5' }),
        0x23 => Some(if shift_pressed { '^' } else { '6' }),
        0x24 => Some(if shift_pressed { '&' } else { '7' }),
        0x25 => Some(if shift_pressed { '*' } else { '8' }),
        0x26 => Some(if shift_pressed { '(' } else { '9' }),
        0x27 => Some(if shift_pressed { ')' } else { '0' }),

        // Enter, Escape, Backspace
        0x28 => Some('\n'),       // Enter
        0x29 => None,              // Escape (no printable)
        0x2A => Some('\x08'),     // Backspace

        // Tab
        0x2B => Some('\t'),       // Tab

        // Spacebar
        0x2C => Some(' '),

        // Hyphen, Equals
        0x2D => Some(if shift_pressed { '_' } else { '-' }),
        0x2E => Some(if shift_pressed { '+' } else { '=' }),

        // Brackets, Backslash
        0x2F => Some(if shift_pressed { '{' } else { '[' }),
        0x30 => Some(if shift_pressed { '}' } else { ']' }),
        0x31 => Some(if shift_pressed { '|' } else { '\\' }),

        // Semicolon, Quotes
        0x33 => Some(if shift_pressed { ':' } else { ';' }),
        0x34 => Some(if shift_pressed { '"' } else { '\'' }),

        // Grave/Tilde
        0x35 => Some(if shift_pressed { '~' } else { '`' }),

        // Comma, Period, Slash
        0x36 => Some(if shift_pressed { '<' } else { ',' }),
        0x37 => Some(if shift_pressed { '>' } else { '.' }),
        0x38 => Some(if shift_pressed { '?' } else { '/' }),

        // Caps Lock
        0x39 => None,

        // Function keys F1-F12
        0x3A..=0x45 => None,

        // Navigation
        0x4C => Some('\x7F'),    // Delete
        0x50 => None,             // Left arrow
        0x4F => None,             // Right arrow
        0x52 => None,             // Up arrow
        0x51 => None,             // Down arrow

        // Keypad
        0x59 => Some('1'),
        0x5A => Some('2'),
        0x5B => Some('3'),
        0x5C => Some('4'),
        0x5D => Some('5'),
        0x5E => Some('6'),
        0x5F => Some('7'),
        0x60 => Some('8'),
        0x61 => Some('9'),
        0x62 => Some('0'),
        0x63 => Some('.'),
        0x67 => Some('='),
        0x68 => None,  // F13-F24
        0x69 => None,
        0x6A => None,
        0x6B => None,
        0x6C => None,
        0x6D => None,
        0x6E => None,
        0x6F => None,
        0x70 => None,
        0x71 => None,
        0x72 => None,
        0x73 => None,

        // Media/System keys
        _ => None,
    }
}

// ========================================================================
// USB HID Boot Protocol — Mouse
// ========================================================================

/// USB HID mouse boot protocol report (3+ bytes)
#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
pub struct MouseReport {
    pub buttons: u8,      // Bit 0: left, Bit 1: right, Bit 2: middle
    pub x_delta: u8,      // X movement (-127 to 127)
    pub y_delta: u8,      // Y movement (-127 to 127)
    // Optional: wheel (byte 3)
    pub wheel: u8,        // Wheel movement (if present)
}

/// Mouse button flags
pub const MOUSE_BTN_LEFT: u8   = 0x01;
pub const MOUSE_BTN_RIGHT: u8  = 0x02;
pub const MOUSE_BTN_MIDDLE: u8 = 0x04;

impl MouseReport {
    pub fn from_bytes(buf: &[u8; 4]) -> Self {
        Self {
            buttons: buf[0],
            x_delta: buf[1],
            y_delta: buf[2],
            wheel: buf[3],
        }
    }

    /// Get signed X delta (cast i8 for proper sign)
    pub fn x_delta_signed(&self) -> i8 {
        self.x_delta as i8
    }

    /// Get signed Y delta
    pub fn y_delta_signed(&self) -> i8 {
        self.y_delta as i8
    }

    pub fn is_left(&self) -> bool {
        self.buttons & MOUSE_BTN_LEFT != 0
    }

    pub fn is_right(&self) -> bool {
        self.buttons & MOUSE_BTN_RIGHT != 0
    }

    pub fn is_middle(&self) -> bool {
        self.buttons & MOUSE_BTN_MIDDLE != 0
    }
}

impl fmt::Debug for MouseReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Mouse(buttons=0x{:04x}, dx={}, dy={})",
            self.buttons, self.x_delta_signed(), self.y_delta_signed())
    }
}

// ========================================================================
// Keyboard State Machine — tracks key presses/releases
// ========================================================================

/// Tracks keyboard state for proper key press/release detection
pub struct KeyboardState {
    previous: KeyboardReport,
    current: KeyboardReport,
}

impl KeyboardState {
    pub fn new() -> Self {
        Self {
            previous: KeyboardReport::default(),
            current: KeyboardReport::default(),
        }
    }

    /// Update with a new keyboard report
    pub fn update(&mut self, report: &KeyboardReport) {
        self.previous = self.current;
        self.current = *report;
    }

    /// Get keys that were just pressed (not in previous report)
    pub fn newly_pressed(&self) -> heapless::Vec<u8, 6> {
        let mut keys = heapless::Vec::new();
        for &new_key in self.current.keys.iter() {
            if new_key != 0 && !self.previous.has_key(new_key) {
                let _ = keys.push(new_key);
            }
        }
        keys
    }

    /// Get keys that were just released
    pub fn newly_released(&self) -> heapless::Vec<u8, 6> {
        let mut keys = heapless::Vec::new();
        for &old_key in self.previous.keys.iter() {
            if old_key != 0 && !self.current.has_key(old_key) {
                let _ = keys.push(old_key);
            }
        }
        keys
    }

    /// Get the first newly-pressed key as a printable character
    pub fn first_char(&self) -> Option<char> {
        let pressed = self.newly_pressed();
        if let Some(&usage) = pressed.first() {
            let shift = self.current.is_shift_pressed();
            hid_usage_to_ascii(usage, shift)
        } else {
            None
        }
    }
}
