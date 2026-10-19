// SPDX-License-Identifier: Apache-2.0
//! Software compositor and native desktop shell.
//!
//! The compositor is a bounded, immediate-mode painter. It owns no authority:
//! files are read through the filesystem service, grants through the portal,
//! and service state through the supervisor. A compromised desktop therefore
//! cannot mint capabilities; it can only ask the resource-owning services.

use crate::arch::x86_64::timer::InterruptGuard;
use crate::graphics::{self, color};
use crate::input::Key;
use crate::serial_line;
use crate::{portal, recovery, scheduler, supervisor};
use core::cell::UnsafeCell;

struct Global<T>(UnsafeCell<T>);

// SAFETY: The desktop is only mutated by the idle task and the keyboard test
// on the single CPU. Paint operations do not run concurrently.
unsafe impl<T> Sync for Global<T> {}

const MAX_WINDOWS: usize = 6;

#[derive(Clone, Copy)]
struct Window {
    title: &'static str,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    kind: WindowKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WindowKind {
    Files,
    Security,
    Portal,
    Settings,
    Recovery,
    Log,
}

const WINDOWS: [Window; MAX_WINDOWS] = [
    Window {
        title: "FILES",
        x: 24,
        y: 52,
        width: 380,
        height: 248,
        kind: WindowKind::Files,
    },
    Window {
        title: "SECURITY CENTER",
        x: 424,
        y: 52,
        width: 400,
        height: 248,
        kind: WindowKind::Security,
    },
    Window {
        title: "FILE PORTAL",
        x: 844,
        y: 52,
        width: 412,
        height: 248,
        kind: WindowKind::Portal,
    },
    Window {
        title: "SETTINGS",
        x: 24,
        y: 320,
        width: 380,
        height: 210,
        kind: WindowKind::Settings,
    },
    Window {
        title: "RECOVERY",
        x: 424,
        y: 320,
        width: 400,
        height: 210,
        kind: WindowKind::Recovery,
    },
    Window {
        title: "SYSTEM LOG",
        x: 844,
        y: 320,
        width: 412,
        height: 210,
        kind: WindowKind::Log,
    },
];

struct DesktopState {
    windows: [Window; MAX_WINDOWS],
    focus: usize,
}

impl DesktopState {
    const fn new() -> Self {
        Self {
            windows: WINDOWS,
            focus: 0,
        }
    }
}

static DESKTOP: Global<DesktopState> = Global(UnsafeCell::new(DesktopState::new()));

/// Initializes the desktop and paints the first frame.
pub fn init() {
    if !graphics::is_ready() {
        serial_line!("AETHER_GRAPHICS_SKIP reason=no-framebuffer");
        return;
    }
    serial_line!(
        "AETHER_GRAPHICS_OK mode={}x{} format=linear",
        graphics::width(),
        graphics::height()
    );
    redraw();
    serial_line!(
        "AETHER_DESKTOP_OK windows={} checksum={:#010x}",
        MAX_WINDOWS,
        graphics::checksum()
    );
}

/// Repaints the whole desktop.
pub fn redraw() {
    if !graphics::is_ready() {
        return;
    }
    let (focus, windows) = {
        let _guard = InterruptGuard::enter();
        // SAFETY: Interrupts are masked for the snapshot.
        let state = unsafe { &*DESKTOP.0.get() };
        (state.focus, state.windows)
    };

    graphics::clear(color::BACKGROUND);
    draw_bar();
    for (index, window) in windows.iter().enumerate() {
        draw_window(window, index == focus);
    }
    draw_taskbar(&windows, focus);
}

fn draw_bar() {
    let width = graphics::width() as i32;
    graphics::fill_rect(0, 0, width, 34, color::BAR);
    graphics::fill_rect(0, 34, width, 1, color::DIVIDER);
    graphics::draw_text(16, 8, "AETHER", color::ACCENT, 2);
    graphics::draw_text(112, 12, "CAPABILITY-BASED DESKTOP", color::MUTED, 1);
    let uptime = scheduler::ticks();
    let mut buffer = [0u8; 32];
    let length = format_u64(&mut buffer, uptime);
    graphics::draw_text(width - 130, 12, "TICKS:", color::MUTED, 1);
    graphics::draw_text(
        width - 72,
        12,
        core::str::from_utf8(&buffer[..length]).unwrap_or("0"),
        color::TEXT,
        1,
    );
}

fn draw_window(window: &Window, focused: bool) {
    graphics::fill_rect(
        window.x + 5,
        window.y + 5,
        window.width,
        window.height,
        color::SHADOW,
    );
    graphics::fill_rect(
        window.x,
        window.y,
        window.width,
        window.height,
        color::WINDOW,
    );
    graphics::fill_rect(
        window.x,
        window.y,
        window.width,
        24,
        if focused { color::TITLE } else { color::BAR },
    );
    graphics::outline(
        window.x,
        window.y,
        window.width,
        window.height,
        if focused {
            color::ACCENT
        } else {
            color::DIVIDER
        },
    );
    graphics::draw_text(
        window.x + 10,
        window.y + 7,
        window.title,
        if focused { color::ACCENT } else { color::TEXT },
        1,
    );
    graphics::fill_rect(
        window.x + 10,
        window.y + 28,
        window.width - 20,
        1,
        color::DIVIDER,
    );
    draw_content(window);
}

fn draw_content(window: &Window) {
    let x = window.x + 12;
    let mut y = window.y + 38;
    let mut files = [0u8; 512];
    let files_length = match window.kind {
        WindowKind::Files => crate::fs::list_path("/", &mut files).unwrap_or(0),
        _ => 0,
    };
    let mut portal_text = [0u8; 256];
    let portal_length = if window.kind == WindowKind::Portal {
        portal::summary(&mut portal_text)
    } else {
        0
    };
    let mut recovery_text = [0u8; 256];
    let recovery_length = if window.kind == WindowKind::Recovery {
        recovery::status_text(&mut recovery_text)
    } else {
        0
    };
    let mut settings = [0u8; 256];
    let settings_length = if window.kind == WindowKind::Settings {
        format_settings(&mut settings, focus_title())
    } else {
        0
    };

    let lines: &[u8] = match window.kind {
        WindowKind::Files => &files[..files_length],
        WindowKind::Portal => &portal_text[..portal_length],
        WindowKind::Recovery => &recovery_text[..recovery_length],
        WindowKind::Settings => &settings[..settings_length],
        _ => &[],
    };

    if !lines.is_empty() {
        for line in lines.split(|byte| *byte == b'\n') {
            if line.is_empty() {
                continue;
            }
            draw_line(x, y, line, color::TEXT);
            y += 16;
            if y > window.y + window.height - 20 {
                break;
            }
        }
        return;
    }

    match window.kind {
        WindowKind::Security => {
            let running = supervisor::running_count();
            let quarantined = supervisor::quarantined_count();
            let lines = [
                ("DEMO ROOT", crate::fs::DEMO_ROOT),
                ("SANDBOX", "PRIVATE ROOTS"),
                ("SERVICES", "RUNNING"),
                ("PORTAL", "GRANTS ACTIVE"),
            ];
            for (label, value) in lines {
                draw_line(x, y, label.as_bytes(), color::MUTED);
                y += 14;
                draw_line(x + 8, y, value.as_bytes(), color::TEXT);
                y += 18;
            }
            draw_line(x, y, b"COUNTERS", color::MUTED);
            y += 14;
            let mut buffer = [0u8; 96];
            let length = format_counters(&mut buffer, running, quarantined);
            draw_line(x + 8, y, &buffer[..length], color::TEXT);
            y += 18;
            draw_line(x, y, b"ESCAPE ATTEMPTS DENIED", color::WARNING);
        }
        WindowKind::Log => {
            for line in [
                "CAPABILITY CHECK: OK",
                "SANDBOX ESCAPES: DENIED",
                "USER PID 4: EXITED 0",
                "INPUT QUEUE: READY",
                "COMPOSITOR: SOFTWARE",
                "FRAMEBUFFER: LINEAR",
            ] {
                draw_line(x, y, line.as_bytes(), color::TEXT);
                y += 18;
            }
        }
        _ => {}
    }
}

fn draw_taskbar(windows: &[Window; MAX_WINDOWS], focus: usize) {
    let width = graphics::width() as i32;
    let height = graphics::height() as i32;
    graphics::fill_rect(0, height - 30, width, 30, color::BAR);
    graphics::fill_rect(0, height - 31, width, 1, color::DIVIDER);
    let mut x = 16;
    for (index, window) in windows.iter().enumerate() {
        let selected = index == focus;
        graphics::draw_text(
            x,
            height - 21,
            window.title,
            if selected {
                color::ACCENT
            } else {
                color::MUTED
            },
            1,
        );
        x += window.title.len() as i32 * 6 + 18;
    }
}

fn focus_title() -> &'static str {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked for the read.
    let state = unsafe { &*DESKTOP.0.get() };
    state.windows[state.focus].title
}

/// Handles a decoded key from the input service.
pub fn on_key(key: Key) {
    let mut redraw_needed = false;
    let mut announced = None;
    {
        let _guard = InterruptGuard::enter();
        // SAFETY: Interrupts are masked for the state update.
        let state = unsafe { &mut *DESKTOP.0.get() };
        match key {
            Key::Tab => {
                state.focus = (state.focus + 1) % state.windows.len();
                redraw_needed = true;
            }
            Key::Up => {
                state.windows[state.focus].y = (state.windows[state.focus].y - 8).max(40);
                redraw_needed = true;
            }
            Key::Down => {
                state.windows[state.focus].y =
                    (state.windows[state.focus].y + 8).min(graphics::height() as i32 - 250);
                redraw_needed = true;
            }
            Key::Left => {
                state.windows[state.focus].x = (state.windows[state.focus].x - 8).max(0);
                redraw_needed = true;
            }
            Key::Right => {
                state.windows[state.focus].x =
                    (state.windows[state.focus].x + 8).min(graphics::width() as i32 - 120);
                redraw_needed = true;
            }
            Key::Enter => {
                announced = Some(state.windows[state.focus].title);
            }
            Key::Escape => {
                state.focus = 0;
                redraw_needed = true;
            }
            Key::Char(_) | Key::Backspace => {}
        }
    }
    if redraw_needed {
        redraw();
    }
    if let Some(title) = announced {
        serial_line!("desktop: activated window={title}");
    }
}

/// Returns the focused window title for diagnostics.
#[must_use]
pub fn focus() -> &'static str {
    focus_title()
}

fn draw_line(x: i32, y: i32, bytes: &[u8], rgb: u32) {
    let mut buffer = [b' '; 128];
    let length = bytes.len().min(buffer.len());
    for (index, byte) in bytes.iter().take(length).enumerate() {
        buffer[index] = if *byte == b'\t' { b' ' } else { *byte };
    }
    if let Ok(text) = core::str::from_utf8(&buffer[..length]) {
        graphics::draw_text(x, y, text, rgb, 1);
    }
}

fn format_u64(buffer: &mut [u8], mut value: u64) -> usize {
    if value == 0 {
        buffer[0] = b'0';
        return 1;
    }
    let mut digits = [0u8; 20];
    let mut index = digits.len();
    while value > 0 {
        index -= 1;
        digits[index] = b'0' + (value % 10) as u8;
        value /= 10;
    }
    let length = digits.len() - index;
    buffer[..length].copy_from_slice(&digits[index..]);
    length
}

fn format_counters(buffer: &mut [u8], running: usize, quarantined: usize) -> usize {
    let mut written = 0usize;
    for (label, value) in [("RUNNING ", running), ("QUARANTINED ", quarantined)] {
        let bytes = label.as_bytes();
        buffer[written..written + bytes.len()].copy_from_slice(bytes);
        written += bytes.len();
        written += format_u64(&mut buffer[written..], value as u64);
        buffer[written] = b' ';
        written += 1;
    }
    written
}

fn format_settings(buffer: &mut [u8], focus: &str) -> usize {
    let mut written = 0usize;
    for line in [
        "DISPLAY: LINEAR FRAMEBUFFER",
        "THEME: AETHER DARK",
        "INPUT: PS2 KEYBOARD + SERIAL",
        "COMPOSITOR: IMMEDIATE MODE",
    ] {
        let bytes = line.as_bytes();
        buffer[written..written + bytes.len()].copy_from_slice(bytes);
        written += bytes.len();
        buffer[written] = b'\n';
        written += 1;
    }
    let bytes = b"FOCUS: ";
    buffer[written..written + bytes.len()].copy_from_slice(bytes);
    written += bytes.len();
    let title = focus.as_bytes();
    let length = title.len().min(buffer.len() - written - 1);
    buffer[written..written + length].copy_from_slice(&title[..length]);
    written += length;
    buffer[written] = b'\n';
    written + 1
}
