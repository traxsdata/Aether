// SPDX-License-Identifier: Apache-2.0
//! Translated PS/2 set-1 keyboard decoding.

use crate::{Key, KeyEvent, Modifiers};

/// Stateful set-1 decoder, including E0 navigation and independent modifiers.
#[derive(Clone, Copy, Debug, Default)]
pub struct Keyboard {
    extended: bool,
    pause_remaining: u8,
    held: u8,
}

impl Keyboard {
    /// Creates a decoder with no keys held.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            extended: false,
            pause_remaining: 0,
            held: 0,
        }
    }

    /// Returns the current modifier state.
    #[must_use]
    pub const fn modifiers(&self) -> Modifiers {
        Modifiers {
            shift: self.held & 0x03 != 0,
            ctrl: self.held & 0x0c != 0,
            alt: self.held & 0x30 != 0,
            super_key: self.held & 0xc0 != 0,
        }
    }

    /// Consumes one byte. Break codes and incomplete prefixes emit no event.
    pub fn push(&mut self, byte: u8) -> Option<KeyEvent> {
        if self.pause_remaining > 0 {
            self.pause_remaining -= 1;
            return None;
        }
        if byte == 0xe1 {
            // Pause is E1 1D 45 E1 9D C5. Do not interpret its fake Ctrl.
            self.extended = false;
            self.pause_remaining = 5;
            return None;
        }
        if byte == 0xe0 {
            self.extended = true;
            return None;
        }
        let extended = core::mem::take(&mut self.extended);
        let released = byte & 0x80 != 0;
        let code = byte & 0x7f;
        let modifier = match (extended, code) {
            (false, 0x2a) => 0x01,
            (false, 0x36) => 0x02,
            (false, 0x1d) => 0x04,
            (true, 0x1d) => 0x08,
            (false, 0x38) => 0x10,
            (true, 0x38) => 0x20,
            (true, 0x5b) => 0x40,
            (true, 0x5c) => 0x80,
            _ => 0,
        };
        if modifier != 0 {
            if released {
                self.held &= !modifier;
            } else {
                self.held |= modifier;
            }
            return None;
        }
        if released {
            return None;
        }
        let key = if extended {
            match code {
                0x48 => Key::Up,
                0x50 => Key::Down,
                0x4b => Key::Left,
                0x4d => Key::Right,
                0x1c => Key::Enter,
                0x35 => Key::Char(b'/'),
                // Print Screen includes fake extended Shift scancodes.
                _ => return None,
            }
        } else {
            decode(code)?
        };
        Some(KeyEvent {
            key,
            modifiers: self.modifiers(),
        })
    }
}

/// Decodes an unextended set-1 make scancode.
#[must_use]
pub fn decode(scancode: u8) -> Option<Key> {
    match scancode {
        0x01 => Some(Key::Escape),
        0x0e => Some(Key::Backspace),
        0x0f => Some(Key::Tab),
        0x1c => Some(Key::Enter),
        0x48 => Some(Key::Up),
        0x50 => Some(Key::Down),
        0x4b => Some(Key::Left),
        0x4d => Some(Key::Right),
        0x3b..=0x44 => Some(Key::Function(scancode - 0x3a)),
        0x57..=0x58 => Some(Key::Function(scancode - 0x4c)),
        0x02..=0x0b => Some(Key::Char(b"1234567890"[(scancode - 0x02) as usize])),
        0x0c => Some(Key::Char(b'-')),
        0x0d => Some(Key::Char(b'=')),
        0x10..=0x19 => Some(Key::Char(b"qwertyuiop"[(scancode - 0x10) as usize])),
        0x1a => Some(Key::Char(b'[')),
        0x1b => Some(Key::Char(b']')),
        0x1e..=0x26 => Some(Key::Char(b"asdfghjkl"[(scancode - 0x1e) as usize])),
        0x27 => Some(Key::Char(b';')),
        0x28 => Some(Key::Char(b'\'')),
        0x29 => Some(Key::Char(b'`')),
        0x2b => Some(Key::Char(b'\\')),
        0x2c..=0x32 => Some(Key::Char(b"zxcvbnm"[(scancode - 0x2c) as usize])),
        0x33 => Some(Key::Char(b',')),
        0x34 => Some(Key::Char(b'.')),
        0x35 => Some(Key::Char(b'/')),
        0x39 => Some(Key::Char(b' ')),
        _ => None,
    }
}
