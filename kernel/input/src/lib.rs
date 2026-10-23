// SPDX-License-Identifier: Apache-2.0
//! Allocation-free input decoding, independent of hardware I/O and UI policy.
//!
//! The kernel owns the ports and IRQ serialization. These pure state machines
//! handle hostile/partial byte streams and can be tested on the host.

#![no_std]
#![forbid(unsafe_code)]

mod keyboard;
mod mouse;
mod supertap;

pub use keyboard::{Keyboard, decode};
pub use mouse::{MouseDecoder, MousePacket};
pub use supertap::{SUPER_TAP_MAX_TICKS, SuperTapTracker};

/// A decoded key. Printable keys use the unshifted ASCII value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// Printable character.
    Char(u8),
    /// Enter/Return.
    Enter,
    /// Backspace.
    Backspace,
    /// Delete (forward delete).
    Delete,
    /// Tab.
    Tab,
    /// Escape.
    Escape,
    /// Arrow up.
    Up,
    /// Arrow down.
    Down,
    /// Arrow left.
    Left,
    /// Arrow right.
    Right,
    /// Function key, one-based.
    Function(u8),
    /// Synthetic event: the Super key was pressed and released alone, quickly,
    /// with no other key in between. Produced by the kernel's input service
    /// (no scancode exists for it); the desktop treats it as the system
    /// search/open chord.
    SuperTap,
}

/// Keyboard modifiers captured at the key's make scancode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    /// Either Control key is held.
    pub ctrl: bool,
    /// Either Alt key is held.
    pub alt: bool,
    /// Either Shift key is held.
    pub shift: bool,
    /// Either platform/Super key is held.
    pub super_key: bool,
}

impl Modifiers {
    /// Returns `true` when the platform command modifier (Super) is held.
    ///
    /// Ctrl and Alt belong to the application, never to the window manager;
    /// only Super is a system modifier. Hosts that grab Super chords can
    /// substitute `Ctrl+Alt`, but that policy lives with the window manager
    /// (see the system-shortcut-key setting), not with the raw decoders.
    #[must_use]
    pub const fn command(self) -> bool {
        self.super_key
    }
}

/// A key and the modifiers held at its make scancode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyEvent {
    /// Decoded key.
    pub key: Key,
    /// Captured modifiers.
    pub modifiers: Modifiers,
}

/// A bounded FIFO. On overflow, retain earlier events and count the lost event.
///
/// Synchronization is the caller's responsibility. Pointer consumers must also
/// reconcile with the latest button snapshot after overflow so a lost release
/// cannot leave a drag stuck indefinitely.
pub struct EventQueue<T: Copy, const N: usize> {
    buffer: [Option<T>; N],
    head: usize,
    len: usize,
    received: u32,
    dropped: u32,
}

impl<T: Copy, const N: usize> EventQueue<T, N> {
    /// Creates an empty queue.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buffer: [None; N],
            head: 0,
            len: 0,
            received: 0,
            dropped: 0,
        }
    }

    /// Enqueues an event, returning `false` when the queue is full.
    pub fn push(&mut self, event: T) -> bool {
        self.received = self.received.saturating_add(1);
        if self.len == N {
            self.dropped = self.dropped.saturating_add(1);
            return false;
        }
        self.buffer[(self.head + self.len) % N] = Some(event);
        self.len += 1;
        true
    }

    /// Removes the oldest queued event.
    pub fn pop(&mut self) -> Option<T> {
        if self.len == 0 {
            return None;
        }
        let event = self.buffer[self.head].take();
        self.head = (self.head + 1) % N;
        self.len -= 1;
        event
    }

    /// Returns `true` when no event is queued.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Number of enqueue attempts, saturating at `u32::MAX`.
    #[must_use]
    pub const fn received(&self) -> u32 {
        self.received
    }

    /// Number of events dropped on overflow, saturating at `u32::MAX`.
    #[must_use]
    pub const fn dropped(&self) -> u32 {
        self.dropped
    }
}

impl<T: Copy, const N: usize> Default for EventQueue<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
