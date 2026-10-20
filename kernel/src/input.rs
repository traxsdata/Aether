// SPDX-License-Identifier: Apache-2.0
//! PS/2 keyboard and mouse input service.
//!
//! IRQ1 and IRQ12 are unmasked after the scheduler timer is configured. The
//! interrupt handlers decode hardware packets and update bounded state; policy
//! (focus, activation, window movement) lives in the desktop service. Modifier
//! state is captured with each key so the desktop can implement shortcuts.

use crate::arch::x86_64::timer::InterruptGuard;
use crate::serial_line;
use aether_wm::Modifiers;
use core::cell::UnsafeCell;

struct Global<T>(UnsafeCell<T>);

// SAFETY: Input state is mutated by the two IRQ handlers and read by the idle
// task. Every read and write masks interrupts or runs in interrupt context on
// the single CPU.
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
    /// Function key, one-based (F1 is 1).
    Function(u8),
}

/// A key and the modifiers held when it was pressed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyEvent {
    /// Decoded key.
    pub key: Key,
    /// Modifiers captured at the make scancode.
    pub modifiers: Modifiers,
}

struct Queue {
    buffer: [Option<KeyEvent>; QUEUE_LEN],
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

    fn push(&mut self, event: KeyEvent) {
        self.received = self.received.saturating_add(1);
        if self.len == QUEUE_LEN {
            self.dropped = self.dropped.saturating_add(1);
            return;
        }
        self.buffer[self.tail] = Some(event);
        self.tail = (self.tail + 1) % QUEUE_LEN;
        self.len += 1;
    }

    fn pop(&mut self) -> Option<KeyEvent> {
        if self.len == 0 {
            return None;
        }
        let event = self.buffer[self.head].take();
        self.head = (self.head + 1) % QUEUE_LEN;
        self.len -= 1;
        event
    }
}

#[derive(Clone, Copy)]
struct MouseState {
    x: i32,
    y: i32,
    buttons: u8,
    width: i32,
    height: i32,
    packet: [u8; 3],
    index: usize,
    packets: u32,
    available: bool,
}

impl MouseState {
    const fn new() -> Self {
        Self {
            x: 0,
            y: 0,
            buttons: 0,
            width: 1,
            height: 1,
            packet: [0; 3],
            index: 0,
            packets: 0,
            available: false,
        }
    }

    fn reset(&mut self, width: i32, height: i32) {
        self.x = width / 2;
        self.y = height / 2;
        self.buttons = 0;
        self.width = width.max(1);
        self.height = height.max(1);
        self.packet = [0; 3];
        self.index = 0;
        self.packets = 0;
    }

    fn push(&mut self, byte: u8) {
        // Bit 3 is fixed in every PS/2 packet's first byte. Use it to recover
        // packet boundaries after a lost/stray byte instead of permanently
        // grouping the stream into the wrong triples.
        if self.index == 0 && byte & 0x08 == 0 {
            return;
        }
        self.packet[self.index] = byte;
        self.index += 1;
        if self.index < 3 {
            return;
        }
        self.index = 0;
        let flags = self.packet[0];
        if flags & 0x08 == 0 {
            return;
        }
        let dx = if flags & 0x10 != 0 {
            i16::from(self.packet[1]) - 256
        } else {
            i16::from(self.packet[1])
        };
        let dy = if flags & 0x20 != 0 {
            i16::from(self.packet[2]) - 256
        } else {
            i16::from(self.packet[2])
        };
        // The device marks an overflowing axis in the header; its movement
        // byte is then saturated and must not be interpreted as a real delta.
        if flags & 0x40 == 0 {
            self.x = (self.x + i32::from(dx)).clamp(0, self.width - 1);
        }
        // PS/2 reports positive Y upward; screen coordinates grow downward.
        if flags & 0x80 == 0 {
            self.y = (self.y - i32::from(dy)).clamp(0, self.height - 1);
        }
        self.buttons = flags & 0x07;
        self.packets = self.packets.saturating_add(1);
    }
}

/// A snapshot of the pointer for the desktop service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MouseSnapshot {
    /// Pointer x coordinate.
    pub x: i32,
    /// Pointer y coordinate.
    pub y: i32,
    /// Button bitmask from the PS/2 packet.
    pub buttons: u8,
    /// Total valid packets received.
    pub packets: u32,
    /// Whether the controller initialized.
    pub available: bool,
}

static QUEUE: Global<Queue> = Global(UnsafeCell::new(Queue::new()));
static MOUSE: Global<MouseState> = Global(UnsafeCell::new(MouseState::new()));
static MODIFIERS: Global<Modifiers> = Global(UnsafeCell::new(Modifiers {
    ctrl: false,
    alt: false,
    shift: false,
    super_key: false,
}));
static EXTENDED: Global<bool> = Global(UnsafeCell::new(false));

const PS2_DATA: u16 = 0x60;
const PS2_STATUS: u16 = 0x64;
const PS2_COMMAND: u16 = 0x64;

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

#[inline]
unsafe fn outb(port: u16, value: u8) {
    // SAFETY: Caller guarantees the port is valid.
    unsafe {
        core::arch::asm!(
            "out dx, al",
            in("dx") port,
            in("al") value,
            options(nomem, nostack, preserves_flags)
        );
    }
}

fn wait_input_empty() -> bool {
    for _ in 0..100_000 {
        // SAFETY: PS/2 status port is fixed on the supported platform.
        if unsafe { inb(PS2_STATUS) } & 0x02 == 0 {
            return true;
        }
    }
    false
}

fn wait_output_full() -> bool {
    for _ in 0..100_000 {
        // SAFETY: PS/2 status port is fixed on the supported platform.
        if unsafe { inb(PS2_STATUS) } & 0x01 != 0 {
            return true;
        }
    }
    false
}

fn flush_output() {
    for _ in 0..16 {
        // SAFETY: PS/2 status and data ports are fixed.
        if unsafe { inb(PS2_STATUS) } & 0x01 == 0 {
            return;
        }
        // SAFETY: Reading the data port clears the output buffer.
        let _ = unsafe { inb(PS2_DATA) };
    }
}

fn controller_command(command: u8) -> bool {
    if !wait_input_empty() {
        return false;
    }
    // SAFETY: Fixed PS/2 command port.
    unsafe { outb(PS2_COMMAND, command) };
    true
}

fn controller_data(data: u8) -> bool {
    if !wait_input_empty() {
        return false;
    }
    // SAFETY: Fixed PS/2 data port.
    unsafe { outb(PS2_DATA, data) };
    true
}

fn read_data() -> Option<u8> {
    if !wait_output_full() {
        return None;
    }
    // SAFETY: Fixed PS/2 data port.
    Some(unsafe { inb(PS2_DATA) })
}

fn mouse_command(command: u8) -> bool {
    if !controller_command(0xd4) || !controller_data(command) {
        return false;
    }
    // A device ACK (0xFA) is expected; treat a timeout as a failed probe.
    matches!(read_data(), Some(0xfa))
}

fn probe_mouse() -> bool {
    flush_output();
    let _ = controller_command(0xad); // disable keyboard
    let _ = controller_command(0xa7); // disable auxiliary port
    flush_output();

    if !controller_command(0x20) {
        return false;
    }
    let Some(mut config) = read_data() else {
        return false;
    };
    // Enable IRQ1 and IRQ12, and allow both device clocks.
    config |= 0x03;
    config &= !0x30;
    if !controller_command(0x60) || !controller_data(config) {
        return false;
    }
    let _ = controller_command(0xa8); // enable auxiliary port
    let _ = controller_command(0xae); // enable keyboard

    if !mouse_command(0xf6) {
        return false; // set defaults
    }
    if !mouse_command(0xf4) {
        return false; // enable data reporting
    }
    true
}

/// Initializes the input queues and PS/2 mouse.
///
/// The keyboard itself was configured by firmware; the controller config byte
/// is updated to enable IRQ12 without changing the scan-code set.
pub fn init(width: u32, height: u32) {
    let _guard = InterruptGuard::enter();
    // SAFETY: This runs once during boot with interrupts masked.
    unsafe {
        *QUEUE.0.get() = Queue::new();
        *MOUSE.0.get() = MouseState::new();
        (*MOUSE.0.get()).reset(width as i32, height as i32);
        *MODIFIERS.0.get() = Modifiers {
            ctrl: false,
            alt: false,
            shift: false,
            super_key: false,
        };
        *EXTENDED.0.get() = false;
    }
    let available = probe_mouse();
    // SAFETY: Still inside the interrupt guard and this is the only writer.
    unsafe {
        (*MOUSE.0.get()).available = available;
    }
    if available {
        serial_line!("input: ps2 mouse ready");
    } else {
        serial_line!("input: ps2 mouse unavailable; keyboard-only");
    }
}

fn current_modifiers() -> Modifiers {
    // SAFETY: The caller holds interrupts or is in interrupt context.
    unsafe { *MODIFIERS.0.get() }
}

fn set_modifier(code: u8, pressed: bool) {
    // SAFETY: Interrupts are masked or this is IRQ context.
    unsafe {
        let modifiers = &mut *MODIFIERS.0.get();
        match code {
            0x2a | 0x36 => modifiers.shift = pressed,
            0x1d => modifiers.ctrl = pressed,
            0x38 => modifiers.alt = pressed,
            0x5b | 0x5c => modifiers.super_key = pressed,
            _ => {}
        }
    }
}

fn handle_scancode(scancode: u8) {
    // SAFETY: Interrupts are masked or this is IRQ context.
    unsafe {
        if scancode == 0xe0 {
            *EXTENDED.0.get() = true;
            return;
        }
        if *EXTENDED.0.get() {
            *EXTENDED.0.get() = false;
            // Extended Control is the right-hand Ctrl key.
            if scancode & 0x7f == 0x1d {
                set_modifier(0x1d, scancode & 0x80 == 0);
            }
            return;
        }
    }
    let released = scancode & 0x80 != 0;
    let code = scancode & 0x7f;
    if matches!(code, 0x2a | 0x36 | 0x1d | 0x38 | 0x5b | 0x5c) {
        set_modifier(code, !released);
        return;
    }
    if released {
        return;
    }
    if let Some(key) = decode(code) {
        let event = KeyEvent {
            key,
            modifiers: current_modifiers(),
        };
        // SAFETY: Interrupts are masked or this is IRQ context.
        unsafe { (*QUEUE.0.get()).push(event) };
    }
}

/// IRQ1 handler: consumes the controller's output buffer.
pub fn on_irq() {
    // SAFETY: IRQ1 is only delivered while the controller and IDT are ready.
    let scancode = unsafe { inb(PS2_DATA) };
    handle_scancode(scancode);
}

/// IRQ12 handler: consumes one mouse packet byte.
pub fn on_mouse_irq() {
    // SAFETY: IRQ12 is only delivered while the controller and IDT are ready.
    let byte = unsafe { inb(PS2_DATA) };
    // SAFETY: This is interrupt context and the state is interrupt-safe.
    unsafe { (*MOUSE.0.get()).push(byte) };
}

/// Drains the next key event.
pub fn next_event() -> Option<KeyEvent> {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked for the queue update.
    unsafe { (*QUEUE.0.get()).pop() }
}

/// Drains queued keys into the desktop service.
pub fn poll() {
    for _ in 0..QUEUE_LEN {
        let Some(event) = next_event() else {
            return;
        };
        crate::desktop::on_key(event.key, event.modifiers);
    }
}

/// Returns a snapshot of the pointer.
#[must_use]
pub fn mouse() -> MouseSnapshot {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked for the read.
    let mouse = unsafe { &*MOUSE.0.get() };
    MouseSnapshot {
        x: mouse.x,
        y: mouse.y,
        buttons: mouse.buttons,
        packets: mouse.packets,
        available: mouse.available,
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

/// Injects a make or break scancode. Used by the boot self-test.
pub fn inject(scancode: u8) {
    handle_scancode(scancode);
}

/// Injects a three-byte PS/2 mouse packet. Used by the boot self-test.
pub fn inject_mouse_packet(packet: [u8; 3]) {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked for the state update.
    unsafe {
        for byte in packet {
            (*MOUSE.0.get()).push(byte);
        }
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
        0x3b..=0x44 => Some(Key::Function(scancode - 0x3a)),
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

/// Runs deterministic keyboard and pointer self-tests.
///
/// The sequence exercises focus, movement, activation, a workspace shortcut,
/// the overview, and its cancellation, leaving the live desktop on workspace
/// one with no overlay open.
pub fn self_test() {
    let before = mouse();
    let decoder_ok = mouse_decoder_self_test();
    // Synthetic motion proves the packet decode path. The controller probe is
    // reported separately because a machine without an auxiliary port must not
    // fail boot.
    inject_mouse_packet([0x08, 12, 2]);
    let after = mouse();
    if !decoder_ok {
        serial_line!("AETHER_POINTER_FAILED reason=decoder-self-test");
    } else if after.x != before.x || after.y != before.y {
        serial_line!(
            "AETHER_POINTER_OK dx={} dy={} packets={}",
            after.x - before.x,
            after.y - before.y,
            after.packets
        );
    } else {
        serial_line!("AETHER_POINTER_FAILED reason=no-motion");
    }

    for scancode in [
        0x0f, // Tab
        0x4d, // Right
        0x50, // Down
        0x1c, // Enter
        0x1d, 0x4d, 0x9d, // Ctrl+Right: next workspace
        0x1d, 0x4b, 0x9d, // Ctrl+Left: previous workspace
        0x3d, // F3: overview
        0x01, // Escape: close overview
    ] {
        inject(scancode);
    }
    let mut decoded = 0usize;
    while let Some(event) = next_event() {
        decoded += 1;
        crate::desktop::on_key(event.key, event.modifiers);
    }
    crate::desktop::restore_default_view();
    serial_line!(
        "AETHER_INPUT_OK keys={} dropped={} queue={} mouse={}",
        decoded,
        dropped(),
        QUEUE_LEN,
        if mouse().available {
            "ps2"
        } else {
            "synthetic"
        }
    );
}

fn mouse_decoder_self_test() -> bool {
    let mut mouse = MouseState::new();
    mouse.reset(100, 100);
    let start = (mouse.x, mouse.y);

    // A stray byte must not start a packet; a valid header must still decode.
    for byte in [0x00, 0x08, 12, 2] {
        mouse.push(byte);
    }
    if (mouse.x, mouse.y) != (start.0 + 12, start.1 - 2) {
        return false;
    }

    // Overflow flags invalidate movement on their respective axes, but the
    // packet remains valid for button-state updates.
    let position = (mouse.x, mouse.y);
    for byte in [0xc9, 0x7f, 0x7f] {
        mouse.push(byte);
    }
    (mouse.x, mouse.y) == position && mouse.buttons == 1 && mouse.packets == 2
}
