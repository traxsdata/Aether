// SPDX-License-Identifier: Apache-2.0
//! Synthetic display endpoint for the Linux compatibility layer.
//!
//! `/dev/aether-display` is one character endpoint per granted Linux task. It
//! carries fixed request records from the in-app Wayland bridge to the ring-0
//! compositor and fixed input records back. It is deliberately not a Wayland
//! wire server: there is no socket, no protocol object graph, no DMA, and no
//! GPU command stream. `present` validates and copies guest pixels into a
//! kernel-owned surface, and the existing compositor paints that surface
//! clipped and scaled into a Linux window.
//!
//! Access is default-deny. The scheduler marks the boot display fixture and
//! the signed GridCraft AppImage identity as display-granted; no native task,
//! service, or unrelated Linux application can open the endpoint.
//!
//! The queue and surface are static and bounded. All access runs on the single
//! CPU under the scheduler/syscall interrupt model; no reference escapes a
//! call.

use crate::arch::x86_64::timer::InterruptGuard;
use crate::input::Key;
use crate::memory::paging;
use crate::serial_line;
use crate::{desktop, graphics, scheduler};
use aether_wm::Rect;
use core::cell::UnsafeCell;
use core::mem::size_of;

/// Request record magic (`"ADTP"` little-endian).
pub const REQUEST_MAGIC: u32 = 0x4144_5450;
/// ABI version accepted by this endpoint.
pub const REQUEST_VERSION: u16 = 1;
/// Fixed request header size in bytes.
pub const REQUEST_SIZE: usize = 48;
/// Fixed event record size in bytes.
pub const EVENT_SIZE: usize = 32;

/// Largest accepted surface width.
pub const MAX_WIDTH: u32 = 1920;
/// Largest accepted surface height.
pub const MAX_HEIGHT: u32 = 1200;
/// Largest accepted presented byte count.
pub const MAX_SURFACE_BYTES: usize = 16 * 1024 * 1024;
/// Largest accepted title.
pub const MAX_TITLE: usize = 64;
/// Bounded input queue; overflow drops the oldest record.
pub const QUEUE_LEN: usize = 32;

const MAX_PIXELS: usize = MAX_WIDTH as usize * MAX_HEIGHT as usize;

// Request kinds.
const KIND_CREATE: u16 = 1;
const KIND_PRESENT: u16 = 2;
const KIND_DESTROY: u16 = 3;
const KIND_SET_TITLE: u16 = 4;

// Event kinds.
const EVENT_CONFIGURE: u32 = 1;
const EVENT_KEY: u32 = 2;
const EVENT_POINTER_MOTION: u32 = 3;
const EVENT_POINTER_BUTTON: u32 = 4;
const EVENT_POINTER_AXIS: u32 = 5;
const EVENT_ENTER: u32 = 6;
const EVENT_LEAVE: u32 = 7;
// Only the Gate 4 GUI test path asks the bridge to close the toplevel; in
// every other build nothing emits this event, so it stays compiled out.
#[cfg(feature = "gate4-gui-test")]
const EVENT_CLOSE: u32 = 8;
const EVENT_FRAME: u32 = 9;

const EACCES: i64 = -13;
const EAGAIN: i64 = -11;
const EBADF: i64 = -9;
const EBUSY: i64 = -16;
const EINVAL: i64 = -22;
const ENOSPC: i64 = -28;
const EFAULT: i64 = -14;

/// One fixed input record.
///
/// The 32-byte guest layout is:
/// `kind(0)` `code(4)` `x(8)` `y(12)` `state(16)` `serial(20)`
/// `modifiers(24)` `reserved(28)`. For pointer events `x`/`y` are in surface
/// pixel space. For key events `state` is 1 for press and 0 for release, and
/// `modifiers` is a bitmask: 1=shift, 2=ctrl, 4=alt, 8=super.
#[derive(Clone, Copy)]
#[repr(C)]
struct DisplayEvent {
    kind: u32,
    code: u32,
    x: i32,
    y: i32,
    state: u32,
    serial: u32,
    /// Modifier bitmask; zero for non-key records.
    modifiers: u32,
    /// Reserved, zero.
    reserved: u32,
}

impl DisplayEvent {
    const EMPTY: Self = Self {
        kind: 0,
        code: 0,
        x: 0,
        y: 0,
        state: 0,
        serial: 0,
        modifiers: 0,
        reserved: 0,
    };

    fn encode(&self) -> [u8; EVENT_SIZE] {
        let mut out = [0u8; EVENT_SIZE];
        out[0..4].copy_from_slice(&self.kind.to_le_bytes());
        out[4..8].copy_from_slice(&self.code.to_le_bytes());
        out[8..12].copy_from_slice(&self.x.to_le_bytes());
        out[12..16].copy_from_slice(&self.y.to_le_bytes());
        out[16..20].copy_from_slice(&self.state.to_le_bytes());
        out[20..24].copy_from_slice(&self.serial.to_le_bytes());
        out[24..28].copy_from_slice(&self.modifiers.to_le_bytes());
        // bytes 28..32 stay zero (reserved)
        out
    }
}

const _: () = assert!(size_of::<DisplayEvent>() == EVENT_SIZE);

struct DisplayState {
    open: bool,
    owner: Option<usize>,
    window: Option<u8>,
    width: u32,
    height: u32,
    stride: u32,
    format: u32,
    title: [u8; MAX_TITLE],
    title_len: usize,
    pixels: [u32; MAX_PIXELS],
    events: [DisplayEvent; QUEUE_LEN],
    head: usize,
    len: usize,
    serial: u32,
    frames: u32,
    presented: bool,
    create_marker: bool,
    present_marker: bool,
    input_marker: bool,
    focused: bool,
    last_pointer: (i32, i32),
    last_buttons: u8,
    keys_read: u32,
    pointer_read: u32,
    focus_read: u32,
    injected: bool,
}

impl DisplayState {
    const fn new() -> Self {
        Self {
            open: false,
            owner: None,
            window: None,
            width: 0,
            height: 0,
            stride: 0,
            format: 0,
            title: [0; MAX_TITLE],
            title_len: 0,
            pixels: [0; MAX_PIXELS],
            events: [DisplayEvent::EMPTY; QUEUE_LEN],
            head: 0,
            len: 0,
            serial: 0,
            frames: 0,
            presented: false,
            create_marker: false,
            present_marker: false,
            input_marker: false,
            focused: false,
            last_pointer: (0, 0),
            last_buttons: 0,
            keys_read: 0,
            pointer_read: 0,
            focus_read: 0,
            injected: false,
        }
    }

    fn clear_surface(&mut self) {
        self.width = 0;
        self.height = 0;
        self.stride = 0;
        self.presented = false;
        self.focused = false;
        self.title_len = 0;
    }

    fn enqueue(&mut self, event: DisplayEvent) {
        self.serial = self.serial.wrapping_add(1);
        let mut event = event;
        event.serial = self.serial;
        if self.len == QUEUE_LEN {
            self.head = (self.head + 1) % QUEUE_LEN;
            self.len -= 1;
        }
        let index = (self.head + self.len) % QUEUE_LEN;
        self.events[index] = event;
        self.len += 1;
        scheduler::linux_wake_pollers();
    }

    fn dequeue(&mut self) -> Option<DisplayEvent> {
        if self.len == 0 {
            return None;
        }
        let event = self.events[self.head];
        self.head = (self.head + 1) % QUEUE_LEN;
        self.len -= 1;
        Some(event)
    }
}

struct Global<T>(UnsafeCell<T>);

// SAFETY: The endpoint is only touched from the single CPU, either with
// interrupts masked (syscall path) or from the desktop idle context. The
// compositor never keeps a borrow across a syscall.
unsafe impl<T> Sync for Global<T> {}

static DISPLAY: Global<DisplayState> = Global(UnsafeCell::new(DisplayState::new()));

fn with_state<R>(f: impl FnOnce(&mut DisplayState) -> R) -> R {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked, so this is the only mutable access.
    unsafe { f(&mut *DISPLAY.0.get()) }
}

fn read_state<R>(f: impl FnOnce(&DisplayState) -> R) -> R {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked, so no writer can run during the call.
    unsafe { f(&*DISPLAY.0.get()) }
}

/// Returns `true` when the current task may own the display endpoint.
#[must_use]
pub fn allowed() -> bool {
    scheduler::current_display_granted()
}

/// Opens the per-task endpoint. A second open in the same task is `EBUSY`.
pub fn open() -> Result<(), i64> {
    if !allowed() {
        return Err(EACCES);
    }
    with_state(|state| {
        if state.open {
            return Err(EBUSY);
        }
        *state = DisplayState::new();
        state.open = true;
        state.owner = Some(scheduler::current_task());
        Ok(())
    })
}

/// Closes the endpoint and retires its window and surface.
pub fn close() {
    let window = with_state(|state| {
        if !state.open {
            return None;
        }
        let window = state.window;
        state.clear_surface();
        state.open = false;
        state.owner = None;
        state.window = None;
        state.len = 0;
        state.head = 0;
        window
    });
    if let Some(id) = window {
        desktop::close_linux_window(id);
    }
}

/// Returns the current window title, or an empty string.
#[must_use]
pub fn title() -> &'static str {
    let _guard = InterruptGuard::enter();
    // SAFETY: The byte array lives in static storage and is only rewritten
    // under the same interrupt guard. The returned borrow is bounded by the
    // desktop paint call, which does not mutate the endpoint.
    unsafe {
        let state = &*DISPLAY.0.get();
        core::str::from_utf8(&state.title[..state.title_len]).unwrap_or("")
    }
}

fn request_u32(data: &[u8], offset: usize) -> u32 {
    let mut raw = [0u8; 4];
    raw.copy_from_slice(&data[offset..offset + 4]);
    u32::from_le_bytes(raw)
}

fn request_u64(data: &[u8], offset: usize) -> u64 {
    let mut raw = [0u8; 8];
    raw.copy_from_slice(&data[offset..offset + 8]);
    u64::from_le_bytes(raw)
}

/// Handles one `write` request record from a Linux task.
///
/// The caller has already copied one bounded syscall buffer into kernel
/// memory; pixel payloads are read directly from the validated guest range.
pub fn write_request(pml4: aether_memory::frame::PhysFrame, data: &[u8]) -> i64 {
    if data.len() < REQUEST_SIZE {
        return EINVAL;
    }
    if request_u32(data, 0) != REQUEST_MAGIC {
        return EINVAL;
    }
    let version = u16::from_le_bytes([data[4], data[5]]);
    let kind = u16::from_le_bytes([data[6], data[7]]);
    let length = request_u32(data, 8) as usize;
    if version != REQUEST_VERSION || data.len() != REQUEST_SIZE + length {
        return EINVAL;
    }
    let width = request_u32(data, 12);
    let height = request_u32(data, 16);
    let stride = request_u32(data, 20);
    let format = request_u32(data, 24);
    // `u64 buffer` is 8-byte aligned in the C record, so it follows four bytes
    // of padding after `format` at offset 32. `flags`/`reserved` sit at 40/44.
    let buffer = request_u64(data, 32);
    let flags = request_u32(data, 40);
    if flags != 0 {
        return EINVAL;
    }
    let mut title = [0u8; MAX_TITLE];
    if matches!(kind, KIND_CREATE | KIND_SET_TITLE) {
        if length > MAX_TITLE {
            return EINVAL;
        }
        title[..length].copy_from_slice(&data[REQUEST_SIZE..REQUEST_SIZE + length]);
        if core::str::from_utf8(&title[..length]).is_err() {
            return EINVAL;
        }
    } else if length != 0 {
        return EINVAL;
    }
    let result = match kind {
        KIND_CREATE => create(width, height, format, &title[..length]),
        KIND_PRESENT => present(pml4, width, height, stride, format, buffer),
        KIND_SET_TITLE => set_title(&title[..length]),
        KIND_DESTROY => {
            destroy();
            0
        }
        _ => return EINVAL,
    };
    if result != 0 {
        serial_line!(
            "AETHER_GATE4_DISPLAY_WRITE_FAIL kind={} code={}",
            kind,
            result
        );
    }
    if result == 0 {
        (REQUEST_SIZE + length) as i64
    } else {
        result
    }
}

fn valid_dimensions(width: u32, height: u32) -> bool {
    (1..=MAX_WIDTH).contains(&width) && (1..=MAX_HEIGHT).contains(&height)
}

fn create(width: u32, height: u32, format: u32, title: &[u8]) -> i64 {
    if !valid_dimensions(width, height) || format > 1 {
        return EINVAL;
    }
    let title = core::str::from_utf8(title).unwrap_or("");
    let window = desktop::open_linux_window(width, height, title);
    with_state(|state| {
        if !state.open {
            return EBADF;
        }
        let Some(window) = window else {
            return ENOSPC;
        };
        state.window = Some(window);
        state.width = width;
        state.height = height;
        state.stride = width * 4;
        state.format = format;
        state.title_len = title.len().min(MAX_TITLE);
        state.title[..state.title_len].copy_from_slice(&title.as_bytes()[..state.title_len]);
        state.presented = false;
        state.focused = false;
        state.enqueue(DisplayEvent {
            kind: EVENT_CONFIGURE,
            code: width,
            x: width as i32,
            y: height as i32,
            state: height,
            ..DisplayEvent::EMPTY
        });
        if !state.create_marker {
            state.create_marker = true;
            serial_line!(
                "AETHER_GATE4_DISPLAY_CREATE_OK pid={} size={}x{}",
                scheduler::current_task(),
                width,
                height
            );
        }
        0
    })
}

fn set_title(title: &[u8]) -> i64 {
    with_state(|state| {
        if !state.open {
            return EBADF;
        }
        if state.window.is_none() {
            return EINVAL;
        }
        state.title_len = title.len().min(MAX_TITLE);
        state.title[..state.title_len].copy_from_slice(&title[..state.title_len]);
        0
    })
}

fn destroy() {
    let window = with_state(|state| {
        let window = state.window.take();
        state.clear_surface();
        window
    });
    if let Some(id) = window {
        desktop::close_linux_window(id);
    }
}

fn present(
    pml4: aether_memory::frame::PhysFrame,
    width: u32,
    height: u32,
    stride: u32,
    format: u32,
    buffer: u64,
) -> i64 {
    let (expected_width, expected_height) = match with_state(|state| {
        if !state.open {
            return Err(EBADF);
        }
        if state.window.is_none() {
            return Err(EINVAL);
        }
        Ok((state.width, state.height))
    }) {
        Ok(value) => value,
        Err(error) => return error,
    };
    // CREATE declares the compositor-side format, but a client may attach
    // either ARGB8888 (1, blended) or XRGB8888 (0, opaque) buffers. Both are
    // accepted; the presented record's format selects the paint path.
    if width != expected_width || height != expected_height || format > 1 {
        return EINVAL;
    }
    let Some(pixel_bytes) = width.checked_mul(4) else {
        return EINVAL;
    };
    if stride < pixel_bytes || !stride.is_multiple_of(4) || buffer == 0 {
        return EINVAL;
    }
    let Some(total) = stride.checked_mul(height) else {
        return EINVAL;
    };
    if total as usize > MAX_SURFACE_BYTES
        || !paging::user_range_valid(pml4, buffer, u64::from(total), false)
    {
        return EFAULT;
    }
    let row_bytes = pixel_bytes as usize;
    let stride_bytes = stride as usize;
    let mut checksum = 0x811c_9dc5u32;
    let result = with_state(|state| {
        for row in 0..height as usize {
            let source = buffer + (row * stride_bytes) as u64;
            let start = row * width as usize;
            let destination = &mut state.pixels[start..start + width as usize];
            // SAFETY: The destination is a kernel-owned slice and the source
            // range was validated as readable user memory. The syscall path is
            // non-preemptible, so the mapping cannot change underneath us.
            let destination_bytes = unsafe {
                core::slice::from_raw_parts_mut(destination.as_mut_ptr() as *mut u8, row_bytes)
            };
            if !paging::read_user_bytes(pml4, source, destination_bytes) {
                return EFAULT;
            }
        }
        for pixel in &state.pixels[..width as usize * height as usize] {
            checksum ^= *pixel;
            checksum = checksum.wrapping_mul(0x0100_0193);
        }
        state.presented = true;
        state.format = format;
        state.frames = state.frames.saturating_add(1);
        // A newly presented frame is damage for the Linux window: the
        // compositor must sample the fresh pixels on its next idle pass.
        crate::desktop::mark_live_dirty();
        if !state.present_marker {
            state.present_marker = true;
            serial_line!(
                "AETHER_GATE4_PRESENT_OK frames={} checksum={:#010x} source=linux",
                state.frames,
                checksum
            );
        }
        // Synthetic input is a boot-fixture hook only. The real AppImage task
        // is driven by the QMP/PS/2 harness and must never receive injected
        // events from the kernel.
        let inject = !state.injected && scheduler::current_task() == scheduler::LINUX_TASK;
        if inject {
            state.injected = true;
        }
        // A frame event tells the bridge that the copy was consumed by the
        // compositor pipeline. It is queued after the present marker so the
        // kernel-owned evidence is already on the serial line.
        state.enqueue(DisplayEvent {
            kind: EVENT_FRAME,
            code: state.frames,
            ..DisplayEvent::EMPTY
        });
        if inject {
            // Exercise the real PS/2 decode and desktop routing path. The
            // idle loop drains these queues into the endpoint, and the guest's
            // subsequent read proves the delivery happened.
            crate::input::inject(0x1e); // 'A' make
            crate::input::inject_mouse_packet([0x08, 6, 4]);
        }
        0
    });
    if result == 0 {
        desktop::mark_live_dirty();
    }
    result
}

/// Asks the bridge to close the toplevel after a completed save.
///
/// This is used by the Gate 4 GUI test path once the portal commit has landed;
/// it is not an application-controlled close.
#[cfg(feature = "gate4-gui-test")]
pub fn request_close() {
    with_state(|state| {
        if state.open && state.window.is_some() {
            state.enqueue(DisplayEvent {
                kind: EVENT_CLOSE,
                ..DisplayEvent::EMPTY
            });
        }
    });
}

/// Drains one or more events into a validated guest buffer.
pub fn read_events(out: &mut [u8]) -> Result<usize, i64> {
    if out.len() < EVENT_SIZE {
        return Err(EINVAL);
    }
    let capacity = (out.len() / EVENT_SIZE).min(QUEUE_LEN);
    with_state(|state| {
        if !state.open {
            return Err(EBADF);
        }
        if state.len == 0 {
            return Err(EAGAIN);
        }
        let mut written = 0usize;
        while written < capacity
            && let Some(event) = state.dequeue()
        {
            let encoded = event.encode();
            out[written..written + EVENT_SIZE].copy_from_slice(&encoded);
            written += EVENT_SIZE;
            match event.kind {
                EVENT_KEY => state.keys_read = state.keys_read.saturating_add(1),
                EVENT_POINTER_MOTION | EVENT_POINTER_BUTTON | EVENT_POINTER_AXIS => {
                    state.pointer_read = state.pointer_read.saturating_add(1);
                }
                EVENT_ENTER | EVENT_CONFIGURE => {
                    state.focus_read = state.focus_read.saturating_add(1);
                }
                _ => {}
            }
        }
        if !state.input_marker
            && state.keys_read > 0
            && state.pointer_read > 0
            && state.focus_read > 0
        {
            state.input_marker = true;
            serial_line!(
                "AETHER_GATE4_INPUT_OK keys={} pointer={} focus={}",
                state.keys_read,
                state.pointer_read,
                state.focus_read
            );
        }
        Ok(written)
    })
}

/// Returns `true` when at least one event is queued.
#[must_use]
pub fn readable() -> bool {
    read_state(|state| state.open && state.len != 0)
}

/// Routes a decoded key while the Linux window owns focus.
///
/// The input decoder emits make events only. The endpoint therefore queues a
/// press immediately followed by a release so a bridge can synthesize both
/// `wl_keyboard.key` transitions without inventing a hold timer in the kernel.
pub fn key(key: Key, modifiers: crate::input::Modifiers) {
    let mut modifier_bits = 0u32;
    if modifiers.shift {
        modifier_bits |= 1;
    }
    if modifiers.ctrl {
        modifier_bits |= 2;
    }
    if modifiers.alt {
        modifier_bits |= 4;
    }
    if modifiers.super_key {
        modifier_bits |= 8;
    }
    let code = match key {
        Key::Char(byte) => u32::from(byte),
        Key::Enter => 0x1000,
        Key::Backspace => 0x1001,
        Key::Delete => 0x1002,
        Key::Tab => 0x1003,
        Key::Escape => 0x1004,
        Key::Up => 0x1005,
        Key::Down => 0x1006,
        Key::Left => 0x1007,
        Key::Right => 0x1008,
        Key::Function(number) => 0x1100 | u32::from(number),
    };
    with_state(|state| {
        if !state.open || state.window.is_none() {
            return;
        }
        state.enqueue(DisplayEvent {
            kind: EVENT_KEY,
            code,
            state: 1,
            modifiers: modifier_bits,
            ..DisplayEvent::EMPTY
        });
        state.enqueue(DisplayEvent {
            kind: EVENT_KEY,
            code,
            state: 0,
            modifiers: modifier_bits,
            ..DisplayEvent::EMPTY
        });
    });
}

/// Maps a pointer coordinate from compositor content pixels to surface pixels.
///
/// The compositor may scale the presented surface into a smaller or larger
/// window, so subtracting the window origin alone is not enough. Clamping keeps
/// a pointer at the frame edge inside the surface.
fn surface_coordinate(value: i32, origin: i32, extent: i32, surface: u32) -> i32 {
    if extent <= 0 || surface == 0 {
        return 0;
    }
    let relative = i64::from(value) - i64::from(origin);
    let scaled = relative.saturating_mul(i64::from(surface)) / i64::from(extent);
    scaled.clamp(0, i64::from(surface) - 1) as i32
}

/// Synchronizes focus and pointer state after the desktop has processed input.
///
/// `focused` is the WM's focused window id; `content` is the Linux window's
/// client area. Coordinates delivered to the guest are in surface pixel space,
/// not compositor window space.
pub fn sync_input(
    focused: Option<u8>,
    content: Option<Rect>,
    pointer_x: i32,
    pointer_y: i32,
    buttons: u8,
) {
    with_state(|state| {
        if !state.open {
            return;
        }
        let Some(window) = state.window else {
            return;
        };
        let Some(content) = content else {
            return;
        };
        let is_focused = focused == Some(window);
        let relative_x = surface_coordinate(pointer_x, content.x, content.width, state.width);
        let relative_y = surface_coordinate(pointer_y, content.y, content.height, state.height);
        if is_focused && !state.focused {
            state.focused = true;
            state.enqueue(DisplayEvent {
                kind: EVENT_CONFIGURE,
                code: state.width,
                x: state.width as i32,
                y: state.height as i32,
                state: state.height,
                ..DisplayEvent::EMPTY
            });
            state.enqueue(DisplayEvent {
                kind: EVENT_ENTER,
                x: relative_x,
                y: relative_y,
                ..DisplayEvent::EMPTY
            });
            // A focus transition also reports the current pointer position so
            // a client that missed earlier motion has one coherent sample.
            state.enqueue(DisplayEvent {
                kind: EVENT_POINTER_MOTION,
                x: relative_x,
                y: relative_y,
                ..DisplayEvent::EMPTY
            });
            state.last_pointer = (relative_x, relative_y);
            state.last_buttons = buttons;
        } else if !is_focused && state.focused {
            state.focused = false;
            state.enqueue(DisplayEvent {
                kind: EVENT_LEAVE,
                x: state.last_pointer.0,
                y: state.last_pointer.1,
                ..DisplayEvent::EMPTY
            });
            state.enqueue(DisplayEvent {
                kind: EVENT_CONFIGURE,
                code: state.width,
                x: state.width as i32,
                y: state.height as i32,
                state: state.height,
                ..DisplayEvent::EMPTY
            });
            state.last_buttons = 0;
        }
        if !state.focused {
            return;
        }
        if state.last_pointer != (relative_x, relative_y) {
            state.last_pointer = (relative_x, relative_y);
            state.enqueue(DisplayEvent {
                kind: EVENT_POINTER_MOTION,
                x: relative_x,
                y: relative_y,
                ..DisplayEvent::EMPTY
            });
        }
        let changed = state.last_buttons ^ buttons;
        if changed & 0x7 != 0 {
            for bit in 0..3 {
                if changed & (1 << bit) != 0 {
                    state.enqueue(DisplayEvent {
                        kind: EVENT_POINTER_BUTTON,
                        code: bit + 1,
                        x: relative_x,
                        y: relative_y,
                        state: u32::from(buttons & (1 << bit) != 0),
                        ..DisplayEvent::EMPTY
                    });
                }
            }
            state.last_buttons = buttons;
        }
    });
}

/// Paints the current surface into a compositor content rectangle.
pub fn paint_content(content: Rect) {
    let available = with_state(|state| {
        if !state.open {
            return None;
        }
        Some((state.presented, state.width, state.height, state.format))
    });
    let Some((presented, width, height, format)) = available else {
        return;
    };
    if !presented || width == 0 || height == 0 {
        graphics::fill_rect(
            content.x,
            content.y,
            content.width,
            content.height,
            0x101722,
        );
        graphics::draw_text(
            content.x + 14,
            content.y + 16,
            "WAITING FOR LINUX SURFACE",
            0x8fa3c8,
            1,
        );
        return;
    }
    let _guard = InterruptGuard::enter();
    // SAFETY: The pixel array is static kernel storage and the paint call is
    // the only reader. Interrupts are masked so `present` cannot rewrite it
    // while the compositor samples it.
    let pixels = unsafe { &(*DISPLAY.0.get()).pixels };
    graphics::blit_surface_scaled(
        content.x,
        content.y,
        content.width,
        content.height,
        &pixels[..width as usize * height as usize],
        width,
        height,
        format,
    );
}
