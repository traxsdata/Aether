// SPDX-License-Identifier: Apache-2.0
//! PS/2 keyboard input service.
//!
//! IRQ1 is unmasked after the scheduler timer is configured. The interrupt
//! handler consumes one scancode and appends a decoded key to a bounded queue;
//! policy (focus, activation, portal consent) lives in the desktop service.

use crate::arch::x86_64::timer::InterruptGuard;
use crate::serial_line;
use core::cell::UnsafeCell;

struct Global<T>(UnsafeCell<T>);

// SAFETY: The queue is mutated by IRQ1 and drained by the idle task. Both
// paths mask interrupts or run in interrupt context on the single CPU.
unsafe impl<T> Sync for Global<T> {}

const QUEUE_LEN: usize = 32;

/// A decoded key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// Printable character.
    Char(u8),
    /// Enter/Return.
    Enter,
    /// Backspace.
    Backspace,
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
}

struct Queue {
    buffer: [Option<Key>; QUEUE_LEN],
    head: usize,
    tail: usize,
    len: usize,
    received: u32,
    dropped: u32,
}

impl Queue {
    const fn new() -> Self {
        Self {
            buffer: [None; QUEUE_LEN],
            head: 0,
            tail: 0,
            len: 0,
            received: 0,
            dropped: 0,
        }
    }

    fn push(&mut self, key: Key) {
        self.received = self.received.saturating_add(1);
        if self.len == QUEUE_LEN {
            self.dropped = self.dropped.saturating_add(1);
            return;
        }
        self.buffer[self.tail] = Some(key);
        self.tail = (self.tail + 1) % QUEUE_LEN;
        self.len += 1;
    }

    fn pop(&mut self) -> Option<Key> {
        if self.len == 0 {
            return None;
        }
        let key = self.buffer[self.head].take();
        self.head = (self.head + 1) % QUEUE_LEN;
        self.len -= 1;
        key
    }
}

static QUEUE: Global<Queue> = Global(UnsafeCell::new(Queue::new()));

#[inline]
unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    // SAFETY: Caller guarantees the port is valid.
    unsafe {
        core::arch::asm!(
            "in al, dx",
            in("dx") port,
            out("al") value,
            options(nomem, nostack, preserves_flags)
        );
    }
    value
}

/// Initializes the input queue. IRQ1 unmasking happens in [`timer::init`].
pub fn init() {
    let _guard = InterruptGuard::enter();
    // SAFETY: This runs once during boot with interrupts masked.
    unsafe { *QUEUE.0.get() = Queue::new() };
}

/// IRQ1 handler: consumes the controller's output buffer.
pub fn on_irq() {
    // SAFETY: IRQ1 is only delivered while the controller and IDT are ready.
    let scancode = unsafe { inb(0x60) };
    if let Some(key) = decode(scancode) {
        // SAFETY: This is interrupt context and the queue is interrupt-safe.
        unsafe { (*QUEUE.0.get()).push(key) };
    }
}

/// Drains the next key.
pub fn next_key() -> Option<Key> {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked for the queue update.
    unsafe { (*QUEUE.0.get()).pop() }
}

/// Drains queued keys into the desktop service.
///
/// The idle loop calls this after boot so physical IRQ1 input reaches the
/// compositor; otherwise scancodes would only fill the queue and be dropped.
pub fn poll() {
    for _ in 0..QUEUE_LEN {
        let Some(key) = next_key() else {
            return;
        };
        crate::desktop::on_key(key);
    }
}

/// Returns the number of decoded keys received.
#[must_use]
pub fn received() -> u32 {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked for the read.
    unsafe { (*QUEUE.0.get()).received }
}

/// Returns the number of dropped keys.
#[must_use]
pub fn dropped() -> u32 {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked for the read.
    unsafe { (*QUEUE.0.get()).dropped }
}

/// Injects a make scancode. Used by the boot self-test.
pub fn inject(scancode: u8) {
    if let Some(key) = decode(scancode) {
        let _guard = InterruptGuard::enter();
        // SAFETY: Interrupts are masked for the queue update.
        unsafe { (*QUEUE.0.get()).push(key) };
    }
}

/// Decodes a set-1 make scancode.
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

/// Runs a deterministic keyboard self-test and drives the desktop focus.
pub fn self_test() {
    for scancode in [0x0f, 0x4d, 0x50, 0x1c, 0x1e, 0x02] {
        inject(scancode);
    }
    let mut decoded = 0usize;
    while let Some(key) = next_key() {
        decoded += 1;
        crate::desktop::on_key(key);
    }
    serial_line!(
        "AETHER_INPUT_OK keys={} dropped={} queue={}",
        decoded,
        dropped(),
        QUEUE_LEN
    );
}
