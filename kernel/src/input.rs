// SPDX-License-Identifier: Apache-2.0
//! PS/2 keyboard and mouse input service.
//!
//! IRQ1 and IRQ12 are unmasked after the scheduler timer is configured. The
//! interrupt handlers decode hardware packets and update bounded state; policy
//! (focus, activation, window movement) lives in the desktop service. Modifier
//! state is captured with each key so the desktop can implement shortcuts.

use crate::arch::x86_64::timer::InterruptGuard;
use crate::serial_line;
use aether_input::{EventQueue, Keyboard, MouseDecoder, SuperTapTracker};
pub use aether_input::{Key, KeyEvent, Modifiers};
use core::cell::UnsafeCell;

struct Global<T>(UnsafeCell<T>);

// SAFETY: Input state is mutated by the two IRQ handlers and read by the idle
// task. Every read and write masks interrupts or runs in interrupt context on
// the single CPU.
unsafe impl<T> Sync for Global<T> {}

const QUEUE_LEN: usize = 32;

type Queue = EventQueue<KeyEvent, QUEUE_LEN>;

#[derive(Clone, Copy)]
struct MouseState {
    x: i32,
    y: i32,
    buttons: u8,
    width: i32,
    height: i32,
    decoder: MouseDecoder,
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
            decoder: MouseDecoder::new(),
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
        self.decoder.reset();
        self.packets = 0;
    }

    fn push(&mut self, byte: u8) -> bool {
        let Some(packet) = self.decoder.push(byte) else {
            return false;
        };
        // PS/2 counts are small at the supported 8 counts/mm resolution and
        // 200 Hz rate; apply a fixed gain so normal pointer travel is usable.
        const MOUSE_GAIN: i32 = 2;
        self.x = self
            .x
            .saturating_add(i32::from(packet.dx).saturating_mul(MOUSE_GAIN))
            .clamp(0, self.width - 1);
        // PS/2 reports positive Y upward; screen coordinates grow downward.
        self.y = self
            .y
            .saturating_sub(i32::from(packet.dy).saturating_mul(MOUSE_GAIN))
            .clamp(0, self.height - 1);
        self.buttons = packet.buttons;
        self.packets = self.packets.saturating_add(1);
        true
    }

    fn snapshot(&self) -> MouseSnapshot {
        MouseSnapshot {
            x: self.x,
            y: self.y,
            buttons: self.buttons,
            packets: self.packets,
            available: self.available,
        }
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
static KEYBOARD: Global<Keyboard> = Global(UnsafeCell::new(Keyboard::new()));
static POINTER_EVENTS: Global<EventQueue<MouseSnapshot, QUEUE_LEN>> =
    Global(UnsafeCell::new(EventQueue::new()));
static SUPER_TAP: Global<SuperTapTracker> = Global(UnsafeCell::new(SuperTapTracker::new()));

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

fn read_data(auxiliary: bool) -> Option<u8> {
    for _ in 0..100_000 {
        // SAFETY: Fixed PS/2 ports; the caller masks interrupts.
        let status = unsafe { inb(PS2_STATUS) };
        if status & 1 == 0 {
            core::hint::spin_loop();
            continue;
        }
        // SAFETY: The status indicates a readable output byte.
        let byte = unsafe { inb(PS2_DATA) };
        if status & 0xc0 == 0 && (status & 0x20 != 0) == auxiliary {
            return Some(byte);
        }
        route_byte(status, byte);
    }
    None
}

fn mouse_command(command: u8) -> bool {
    for _ in 0..3 {
        if !controller_command(0xd4) || !controller_data(command) {
            return false;
        }
        match read_data(true) {
            Some(0xfa) => return true,
            Some(0xfe) => continue, // bounded device-requested resend
            _ => return false,
        }
    }
    false
}

fn probe_mouse() -> bool {
    let available = probe_mouse_inner();
    // Every exit, including a missing auxiliary port or a command timeout,
    // attempts to re-enable the keyboard clock. Never strand it after 0xAD.
    let keyboard_enabled = controller_command(0xae);
    if !available {
        let _ = controller_command(0xa7);
    }
    available && keyboard_enabled
}

fn probe_mouse_inner() -> bool {
    flush_output();
    if !controller_command(0xad) || !controller_command(0xa7) {
        return false;
    }
    flush_output();

    if !controller_command(0x20) {
        return false;
    }
    let Some(mut config) = read_data(false) else {
        return false;
    };
    // Preserve firmware's translation mode, enable both interrupt lines, and
    // leave both device clocks enabled. Firmware may hand over with a clock
    // disabled, which silently produces ACKs during probing but no IRQ12
    // traffic on some hypervisors.
    config |= 0x03;
    config &= !0x30;
    if !controller_command(0x60) || !controller_data(config) {
        return false;
    }
    if !controller_command(0xa8) {
        return false;
    }

    configure_mouse_reporting()
}

/// Programs a known mouse reporting mode.
///
/// Defaults are not enough on every virtual PS/2 controller: set the highest
/// standard sample rate and 8 counts/mm before enabling reporting.
fn configure_mouse_reporting() -> bool {
    if !mouse_command(0xf6) {
        return false; // set defaults
    }
    if !mouse_command_with_argument(0xf3, 200) {
        return false; // set sample rate
    }
    if !mouse_command_with_argument(0xe8, 0x03) {
        return false; // set resolution to 8 counts/mm
    }
    if !mouse_command(0xf4) {
        return false; // enable data reporting
    }
    true
}

/// Recovers the mouse after a long interrupt-disabled boot phase.
///
/// Kernel imports can run for seconds with interrupts masked. A user moving
/// the mouse during that window overruns the controller's one-byte output
/// register and desynchronizes the packet decoder. Discard stale bytes, reset
/// the decoder, and re-send the reporting configuration before the Linux
/// window starts accepting input.
pub fn rearm_mouse() {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked and this is the only writer.
    unsafe {
        if !(*MOUSE.0.get()).available {
            return;
        }
        flush_output();
        (*MOUSE.0.get()).decoder.reset();
    }
    let ok = configure_mouse_reporting();
    serial_line!("input: ps2 mouse rearmed={}", ok);
}

/// Sends a two-byte mouse command and waits for the device ACK.
fn mouse_command_with_argument(command: u8, argument: u8) -> bool {
    if !mouse_command(command) {
        return false;
    }
    if !controller_command(0xd4) || !controller_data(argument) {
        return false;
    }
    matches!(read_data(true), Some(0xfa))
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
        *KEYBOARD.0.get() = Keyboard::new();
        *POINTER_EVENTS.0.get() = EventQueue::new();
        *SUPER_TAP.0.get() = SuperTapTracker::new();
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

/// Decodes one scancode, queues any decoded key event, and drives the
/// Super-tap tracker. A make for any key other than Super while Super is
/// held (a regular key or a modifier) cancels the tap; a quick, unbroken
/// Super release emits the synthetic `Key::SuperTap`.
fn handle_scancode(scancode: u8) {
    // SAFETY: Interrupts are masked or this is IRQ context.
    unsafe {
        let before = (*KEYBOARD.0.get()).modifiers();
        let produced = (*KEYBOARD.0.get()).push(scancode).map(|event| {
            (*QUEUE.0.get()).push(event);
            true
        });
        let after = (*KEYBOARD.0.get()).modifiers();
        let now = crate::scheduler::ticks();
        let produced_key = matches!(produced, Some(true));
        if !before.super_key && after.super_key {
            // Super went down: start the tap window.
            (*SUPER_TAP.0.get()).super_down(now);
        } else if before.super_key && !after.super_key {
            // The last Super key went up. A quick release with no key in
            // between is the system tap (search/open), like the Start menu.
            if (*SUPER_TAP.0.get()).super_up(now) {
                (*QUEUE.0.get()).push(KeyEvent {
                    key: Key::SuperTap,
                    modifiers: Modifiers::default(),
                });
            }
        } else if after.super_key && (produced_key || other_modifier_made(before, after)) {
            // A key other than Super went down while Super is held: a
            // regular key (decoded event) or a Ctrl/Alt/Shift make, which
            // produces no event. Either way, Super+key is a chord, not a
            // tap.
            (*SUPER_TAP.0.get()).other_down();
        }
    }
}

/// Returns `true` when a non-Super modifier was made between two samples.
/// Modifier makes never produce a decoded key event, so the tracker, which
/// only sees events, needs them reported explicitly (a Super+Ctrl hold is a
/// chord, not a tap).
const fn other_modifier_made(before: Modifiers, after: Modifiers) -> bool {
    (after.shift && !before.shift) || (after.ctrl && !before.ctrl) || (after.alt && !before.alt)
}

fn handle_mouse(byte: u8) {
    // SAFETY: Interrupts are masked or this is IRQ context; the two globals
    // have disjoint storage and no borrow escapes this function.
    unsafe {
        let mouse = &mut *MOUSE.0.get();
        if mouse.push(byte) {
            (*POINTER_EVENTS.0.get()).push(mouse.snapshot());
        }
    }
}

fn route_byte(status: u8, byte: u8) {
    if status & 0xc0 != 0 {
        // SAFETY: The caller masks interrupts or is an IRQ handler. A dropped
        // byte invalidates partial packets and can include a modifier release.
        unsafe {
            if status & 0x20 != 0 {
                (*MOUSE.0.get()).decoder.reset();
            } else {
                *KEYBOARD.0.get() = Keyboard::new();
            }
        }
    } else if status & 0x20 != 0 {
        handle_mouse(byte);
    } else {
        handle_scancode(byte);
    }
}

/// IRQ1 handler: drains bounded output using the controller's source bit.
pub fn on_irq() {
    // IRQ1 and IRQ12 can be pending together. The vector alone does not tell
    // which device currently owns the shared output register.
    for _ in 0..QUEUE_LEN {
        // SAFETY: Fixed ports, accessed only in interrupt context.
        let status = unsafe { inb(PS2_STATUS) };
        if status & 1 == 0 {
            break;
        }
        // SAFETY: A byte is available according to the status just read.
        route_byte(status, unsafe { inb(PS2_DATA) });
    }
}

/// IRQ12 handler: shares the source-checked drain path with IRQ1.
pub fn on_mouse_irq() {
    on_irq();
}

/// Drains the PS/2 output register while interrupts are masked.
///
/// Surface copies and the compositor hold interrupts off for millions of
/// pixel operations. The controller has a one-byte output register, so a key
/// or mouse packet arriving in that window is otherwise lost. Long loops call
/// this between chunks to keep input loss bounded.
pub fn service_controller() {
    for _ in 0..QUEUE_LEN {
        // SAFETY: Fixed ports, accessed with interrupts already masked by the
        // caller (or from interrupt context).
        let status = unsafe { inb(PS2_STATUS) };
        if status & 1 == 0 {
            return;
        }
        // SAFETY: A byte is available according to the status just read.
        route_byte(status, unsafe { inb(PS2_DATA) });
    }
}

/// Drains the next key event.
pub fn next_event() -> Option<KeyEvent> {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked for the queue update.
    unsafe { (*QUEUE.0.get()).pop() }
}

/// Returns `true` when pointer motion is waiting for the desktop service.
pub fn pointer_pending() -> bool {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked while the queue state is sampled.
    unsafe { !(*POINTER_EVENTS.0.get()).is_empty() }
}

/// Returns `true` when a key is waiting for the desktop/shell service.
pub fn key_pending() -> bool {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked while the queue state is sampled.
    unsafe { !(*QUEUE.0.get()).is_empty() }
}

/// Returns the modifier keys currently held, as decoded by the keyboard.
///
/// Surfacing this to the shell diagnostics lets a test harness wait for a
/// modifier release to land before it asserts on plain keys: a Super that
/// still reads as held would otherwise silently route plain arrows and
/// letters as system chords.
#[must_use]
pub fn held_modifiers() -> Modifiers {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked for the read.
    unsafe { (*KEYBOARD.0.get()).modifiers() }
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
    unsafe { (*MOUSE.0.get()).snapshot() }
}

/// Atomically drains a bounded batch and captures the final pointer state.
/// The final snapshot reconciles overflow without reordering a later IRQ batch.
pub fn drain_mouse_events() -> ([Option<MouseSnapshot>; QUEUE_LEN], MouseSnapshot) {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked for both the drain and matching snapshot.
    unsafe {
        let events = core::array::from_fn(|_| (*POINTER_EVENTS.0.get()).pop());
        (events, (*MOUSE.0.get()).snapshot())
    }
}

/// Returns the number of pointer events dropped on queue exhaustion.
#[must_use]
pub fn pointer_dropped() -> u32 {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked for the read.
    unsafe { (*POINTER_EVENTS.0.get()).dropped() }
}

/// Returns the number of decoded keys received.
#[must_use]
pub fn received() -> u32 {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked for the read.
    unsafe { (*QUEUE.0.get()).received() }
}

/// Returns the number of dropped keys.
#[must_use]
pub fn dropped() -> u32 {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked for the read.
    unsafe { (*QUEUE.0.get()).dropped() }
}

/// Injects a make or break scancode. Used by the boot self-test.
pub fn inject(scancode: u8) {
    let _guard = InterruptGuard::enter();
    handle_scancode(scancode);
}

/// Injects a three-byte PS/2 mouse packet. Used by the boot self-test.
pub fn inject_mouse_packet(packet: [u8; 3]) {
    let _guard = InterruptGuard::enter();
    for byte in packet {
        handle_mouse(byte);
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
        0xe0, 0x4d, 0xe0, 0xcd, // dedicated Right, make/break
        0xe0, 0x50, 0xe0, 0xd0, // dedicated Down, make/break
        0x1c, // Enter
        // Super+Alt+Right: a key is decoded between Super make and break, so
        // the chord must not read as a Super-tap.
        0xe0, 0x5c, 0x38, 0xe0, 0x4d, 0xe0, 0xcd, 0xb8, 0xe0, 0xdc,
        // Super+Alt+Left: back to the first workspace.
        0xe0, 0x5b, 0x38, 0xe0, 0x4b, 0xe0, 0xcb, 0xb8, 0xe0, 0xdb, 0x3d, // F3: overview
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
    if decoded != 8 || dropped() != 0 {
        serial_line!("AETHER_INPUT_FAILED keys={} dropped={}", decoded, dropped());
        return;
    }
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
    if (mouse.x, mouse.y) != (start.0 + 24, start.1 - 4) {
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
