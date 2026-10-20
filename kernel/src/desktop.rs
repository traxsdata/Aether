// SPDX-License-Identifier: Apache-2.0
//! Software compositor and native desktop shell.
//!
//! The compositor is a bounded, immediate-mode painter. It owns no authority:
//! files are read through the filesystem service, grants through the portal,
//! and service state through the supervisor. A compromised desktop therefore
//! cannot mint capabilities; it can only ask the resource-owning services.

use crate::arch::x86_64::serial;
use crate::arch::x86_64::timer::InterruptGuard;
use crate::graphics::{self, color};
use crate::input::Key;
use crate::serial_line;
use crate::{fs, packaging, portal, recovery, scheduler, supervisor, syscall, updates};
use aether_gridcraft::{self as gridcraft, AppKey};
use aether_packaging::manifest::Version;
use core::cell::UnsafeCell;

struct Global<T>(UnsafeCell<T>);

// SAFETY: The desktop is only mutated by the idle task and the keyboard test
// on the single CPU. Paint operations do not run concurrently.
unsafe impl<T> Sync for Global<T> {}

const MAX_WINDOWS: usize = 7;

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
    GridCraft,
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
    Window {
        title: "GRIDCRAFT",
        x: 24,
        y: 552,
        width: 1232,
        height: 216,
        kind: WindowKind::GridCraft,
    },
];

struct DesktopState {
    windows: [Window; MAX_WINDOWS],
    focus: usize,
    gridcraft: gridcraft::App,
}

impl DesktopState {
    const fn new() -> Self {
        Self {
            windows: WINDOWS,
            focus: 0,
            gridcraft: gridcraft::App::new(),
        }
    }
}

static DESKTOP: Global<DesktopState> = Global(UnsafeCell::new(DesktopState::new()));

/// Initializes the desktop and paints the first frame.
pub fn init() {
    let report = gridcraft::self_test();
    if report.failures == 0 {
        serial_line!(
            "AETHER_GRIDCRAFT_OK checks={} failures=0 sum_milli={}",
            report.checks,
            report.sum_milli
        );
    } else {
        serial_line!(
            "gridcraft: self-test failed checks={} failures={}",
            report.checks,
            report.failures
        );
    }
    {
        let _guard = InterruptGuard::enter();
        // SAFETY: Interrupts are masked and this runs once during boot.
        let state = unsafe { &mut *DESKTOP.0.get() };
        state.gridcraft = gridcraft::App::sample();
    }
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
    let (focus, windows, gridcraft) = {
        let _guard = InterruptGuard::enter();
        // SAFETY: Interrupts are masked for the snapshot.
        let state = unsafe { &*DESKTOP.0.get() };
        (state.focus, state.windows, state.gridcraft)
    };

    graphics::clear(color::BACKGROUND);
    draw_bar();
    for (index, window) in windows.iter().enumerate() {
        draw_window(window, index == focus, &gridcraft);
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

fn draw_window(window: &Window, focused: bool, gridcraft: &gridcraft::App) {
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
    draw_content(window, gridcraft);
}

fn draw_content(window: &Window, gridcraft: &gridcraft::App) {
    if window.kind == WindowKind::GridCraft {
        draw_gridcraft(window, gridcraft);
        return;
    }
    let x = window.x + 12;
    let mut y = window.y + 38;
    let mut files = [0u8; 512];
    let files_length = match window.kind {
        WindowKind::Files => fs::list_path("/", &mut files).unwrap_or(0),
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
    let mut security = [0u8; 256];
    let security_length = if window.kind == WindowKind::Security {
        format_security(&mut security)
    } else {
        0
    };
    let mut log = [0u8; 256];
    let log_length = if window.kind == WindowKind::Log {
        format_log(&mut log)
    } else {
        0
    };

    let lines: &[u8] = match window.kind {
        WindowKind::Files => &files[..files_length],
        WindowKind::Portal => &portal_text[..portal_length],
        WindowKind::Recovery => &recovery_text[..recovery_length],
        WindowKind::Settings => &settings[..settings_length],
        WindowKind::Security => &security[..security_length],
        WindowKind::Log => &log[..log_length],
        WindowKind::GridCraft => &[],
    };

    if lines.is_empty() {
        return;
    }
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
    draw_denied_events(window, x, y);
}

/// Paints the GridCraft spreadsheet window.
///
/// Geometry is derived from the window rectangle and every draw is bounded by
/// the sheet dimensions. The application model evaluates formulas; the
/// compositor only displays values it returns.
fn draw_gridcraft(window: &Window, app: &gridcraft::App) {
    use aether_gridcraft::{COLS, ROWS};

    let left = window.x + 10;
    let right = window.x + window.width - 10;
    let top = window.y + 32;
    let row_header = 28;
    let cell_width = ((right - left - row_header) / COLS as i32).max(24);
    let cell_height = 14;
    let header_height = 14;
    let grid_top = top + 22;
    let total_width = row_header + cell_width * COLS as i32;

    // Name box and formula bar.
    graphics::fill_rect(left, top, 40, 18, color::GRID_HEADER);
    graphics::outline(left, top, 40, 18, color::GRID_LINE);
    let mut address = [0u8; 8];
    let address_length = app.address(&mut address);
    if let Ok(text) = core::str::from_utf8(&address[..address_length]) {
        graphics::draw_text(left + 8, top + 5, text, color::ACCENT, 1);
    }

    let bar_x = left + 44;
    let bar_width = right - bar_x;
    graphics::fill_rect(bar_x, top, bar_width, 18, color::GRID_CELL);
    graphics::outline(bar_x, top, bar_width, 18, color::GRID_LINE);
    let mut editor = [0u8; 64];
    let source = app.editor_text();
    let copy = source.len().min(editor.len());
    editor[..copy].copy_from_slice(&source[..copy]);
    let mut editor_length = copy;
    if app.is_editing() && editor_length < editor.len() {
        editor[editor_length] = b'_';
        editor_length += 1;
    }
    if let Ok(text) = core::str::from_utf8(&editor[..editor_length]) {
        let max_chars = ((bar_width - 76) / 6).max(1) as usize;
        draw_clipped_text(bar_x + 6, top + 5, text, color::TEXT, max_chars);
    }
    if let Ok(text) = core::str::from_utf8(app.status().as_bytes()) {
        let x = bar_x + bar_width - 8 - text.len() as i32 * 6;
        graphics::draw_text(x, top + 5, text, color::MUTED, 1);
    }

    // Column and row headers.
    graphics::fill_rect(
        left,
        grid_top,
        total_width,
        header_height,
        color::GRID_HEADER,
    );
    for column in 0..COLS {
        let x = left + row_header + column as i32 * cell_width;
        if column == app.column() {
            graphics::fill_rect(x, grid_top, cell_width, header_height, color::GRID_SELECT);
        }
        let label = [b'A' + column as u8];
        if let Ok(text) = core::str::from_utf8(&label) {
            graphics::draw_text(x + cell_width / 2 - 3, grid_top + 4, text, color::TEXT, 1);
        }
    }
    let mut buffer = [0u8; 32];
    for row in 0..ROWS {
        let y = grid_top + header_height + row as i32 * cell_height;
        if row == app.row() {
            graphics::fill_rect(left, y, row_header, cell_height, color::GRID_SELECT);
        }
        let length = format_u64(&mut buffer, (row + 1) as u64);
        if let Ok(text) = core::str::from_utf8(&buffer[..length]) {
            graphics::draw_text(
                left + row_header - 6 - text.len() as i32 * 6,
                y + 4,
                text,
                if row == app.row() {
                    color::ACCENT
                } else {
                    color::MUTED
                },
                1,
            );
        }
        for column in 0..COLS {
            let x = left + row_header + column as i32 * cell_width;
            let selected = row == app.row() && column == app.column();
            graphics::fill_rect(
                x,
                y,
                cell_width,
                cell_height,
                if selected {
                    color::GRID_SELECT
                } else {
                    color::GRID_CELL
                },
            );
            let mut display = [0u8; 24];
            let display_length = app.sheet.display(row, column, &mut display);
            if display_length > 0
                && let Ok(text) = core::str::from_utf8(&display[..display_length])
            {
                let max_chars = ((cell_width - 6) / 6).max(1) as usize;
                let rgb = if display.starts_with(b"ERR:") {
                    color::WARNING
                } else if column == 3 && row > 0 {
                    color::ACCENT
                } else {
                    color::TEXT
                };
                draw_clipped_text(x + 3, y + 4, text, rgb, max_chars);
            }
        }
    }

    // Grid lines and selection outline.
    for column in 0..=COLS {
        let x = left + row_header + column as i32 * cell_width;
        graphics::fill_rect(
            x,
            grid_top,
            1,
            header_height + ROWS as i32 * cell_height,
            color::GRID_LINE,
        );
    }
    for row in 0..=ROWS {
        let y = grid_top + header_height + row as i32 * cell_height;
        graphics::fill_rect(left, y, total_width, 1, color::GRID_LINE);
    }
    graphics::fill_rect(left, grid_top, row_header, 1, color::GRID_LINE);
    let selection_x = left + row_header + app.column() as i32 * cell_width;
    let selection_y = grid_top + header_height + app.row() as i32 * cell_height;
    graphics::outline(
        selection_x,
        selection_y,
        cell_width,
        cell_height,
        color::ACCENT,
    );
}

fn draw_clipped_text(x: i32, y: i32, text: &str, rgb: u32, max_chars: usize) {
    let length = text.len().min(max_chars);
    if let Some(clipped) = text.get(..length) {
        graphics::draw_text(x, y, clipped, rgb, 1);
    }
}

/// Closes the security center and system log with a live roll-up of denied
/// events, drawn in the warning colour so denials read as incidents rather
/// than as ordinary status text.
fn draw_denied_events(window: &Window, x: i32, y: i32) {
    if window.kind != WindowKind::Security && window.kind != WindowKind::Log {
        return;
    }
    if y > window.y + window.height - 20 {
        return;
    }
    let mut buffer = [0u8; 96];
    let mut writer = LineWriter::new(&mut buffer);
    writer.push(b"DENIED EVENTS ESCAPES=");
    writer.value(fs::escape_denials());
    writer.push(b" ACCESS=");
    writer.value(fs::access_denials());
    writer.push(b" QUOTA=");
    writer.value(fs::quota_denials());
    writer.push(b" IPC=");
    writer.value(syscall::ipc_denials());
    let length = writer.length();
    draw_line(x, y, &buffer[..length], color::WARNING);
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
        let gridcraft_focused = state.windows[state.focus].kind == WindowKind::GridCraft;
        if key == Key::Tab {
            // Tab always reaches the compositor so every window stays
            // reachable; an active grid edit is committed first.
            if gridcraft_focused {
                state.gridcraft.commit();
            }
            state.focus = (state.focus + 1) % state.windows.len();
            redraw_needed = true;
        } else if gridcraft_focused {
            if key == Key::Escape && !state.gridcraft.is_editing() {
                state.focus = 0;
                redraw_needed = true;
            } else if let Some(app_key) = gridcraft_key(key) {
                state.gridcraft.on_key(app_key);
                redraw_needed = true;
            }
        } else {
            match key {
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
                Key::Char(_) | Key::Backspace | Key::Tab => {}
            }
        }
    }
    if redraw_needed {
        redraw();
    }
    if let Some(title) = announced {
        serial_line!("desktop: activated window={title}");
    }
}

fn gridcraft_key(key: Key) -> Option<AppKey> {
    match key {
        Key::Char(character) => Some(AppKey::Char(character)),
        Key::Enter => Some(AppKey::Enter),
        Key::Backspace => Some(AppKey::Backspace),
        Key::Escape => Some(AppKey::Escape),
        Key::Up => Some(AppKey::Up),
        Key::Down => Some(AppKey::Down),
        Key::Left => Some(AppKey::Left),
        Key::Right => Some(AppKey::Right),
        Key::Tab => None,
    }
}

/// Prints the GridCraft sheet as CSV and a bounded status line.
///
/// The console shell is a diagnostic surface with kernel authority, so this
/// only reads the running application's public model.
pub fn print_gridcraft() {
    let mut buffer = [0u8; 1024];
    let mut address = [0u8; 8];
    let (length, used, editing, checksum, address_length) = {
        let _guard = InterruptGuard::enter();
        // SAFETY: Interrupts are masked for the snapshot.
        let app = unsafe { &*DESKTOP.0.get() }.gridcraft;
        let length = app.sheet.write_csv(&mut buffer);
        let mut checksum = 0x811c_9dc5u32;
        for byte in &buffer[..length] {
            checksum ^= u32::from(*byte);
            checksum = checksum.wrapping_mul(0x0100_0193);
        }
        (
            length,
            app.used_cells(),
            app.is_editing(),
            checksum,
            app.address(&mut address),
        )
    };
    serial::write_bytes(&buffer[..length]);
    serial_line!(
        "gridcraft: used={} selected={} mode={} checksum={:#010x}",
        used,
        core::str::from_utf8(&address[..address_length]).unwrap_or("?"),
        if editing { "edit" } else { "ready" },
        checksum
    );
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

/// A bounded writer for window text; it truncates instead of panicking and
/// never reads kernel memory beyond what its caller handed it.
struct LineWriter<'a> {
    buffer: &'a mut [u8],
    written: usize,
}

impl<'a> LineWriter<'a> {
    fn new(buffer: &'a mut [u8]) -> Self {
        Self { buffer, written: 0 }
    }

    fn push(&mut self, bytes: &[u8]) {
        let room = self.buffer.len().saturating_sub(self.written);
        let length = bytes.len().min(room);
        self.buffer[self.written..self.written + length].copy_from_slice(&bytes[..length]);
        self.written += length;
    }

    /// Appends a decimal value.
    fn value(&mut self, value: usize) {
        let mut digits = [0u8; 20];
        let length = format_u64(&mut digits, value as u64);
        self.push(&digits[..length]);
    }
    /// Terminates the current line.
    fn end_line(&mut self) {
        if self.written < self.buffer.len() {
            self.buffer[self.written] = b'\n';
            self.written += 1;
        }
    }

    /// Returns the number of written bytes.
    fn length(self) -> usize {
        self.written
    }
}

fn push_version(writer: &mut LineWriter, version: Version) {
    writer.value(usize::from(version.major));
    writer.push(b".");
    writer.value(usize::from(version.minor));
    writer.push(b".");
    writer.value(usize::from(version.patch));
}

/// Formats the SECURITY CENTER content from live kernel counters.
///
/// Every line is derived from state the kernel itself recorded during the
/// boot self-test. The desktop cannot write files, so nothing here is
/// authored or cached.
fn format_security(buffer: &mut [u8]) -> usize {
    let mut writer = LineWriter::new(buffer);
    match packaging::installed() {
        Some(installed) => {
            writer.push(b"PACKAGE ");
            writer.push(installed.id.as_bytes());
            writer.push(b" ");
            push_version(&mut writer, installed.version);
            writer.end_line();
            writer.push(b"ROOT ");
            writer.push(installed.root.as_bytes());
            writer.end_line();
        }
        None => {
            writer.push(b"PACKAGE NONE VERIFIED");
            writer.end_line();
        }
    }
    writer.push(b"SERVICES RUNNING=");
    writer.value(supervisor::running_count());
    writer.push(b" QUARANTINED=");
    writer.value(supervisor::quarantined_count());
    writer.end_line();
    writer.push(b"ISOLATION OTHER=");
    writer.push(fs::OTHER_ROOT.as_bytes());
    writer.end_line();
    writer.push(b"PORTAL GRANTS=");
    writer.value(portal::grant_count());
    writer.push(b" USES=");
    writer.value(portal::grant_uses());
    writer.push(b" DENIED=");
    writer.value(portal::denials());
    writer.end_line();
    writer.push(b"USERS EXITS=");
    writer.value(scheduler::user_exits());
    writer.push(b" FAULTS=");
    writer.value(scheduler::user_faults());
    writer.end_line();
    writer.push(b"IPC DENIED=");
    writer.value(syscall::ipc_denials());
    writer.end_line();
    match updates::status() {
        Some(status) => {
            writer.push(b"UPDATE ");
            push_version(&mut writer, status.installed);
            writer.push(b" CTR ");
            writer.value(usize::try_from(status.installed_counter).unwrap_or(0));
            writer.push(b" CH ");
            writer.push(status.channel.name().as_bytes());
            writer.end_line();
        }
        None => {
            writer.push(b"UPDATE NONE");
            writer.end_line();
        }
    }
    writer.length()
}

/// Formats the SYSTEM LOG from the same live counters the security center
/// uses; the log only reports what already happened.
fn format_log(buffer: &mut [u8]) -> usize {
    let mut writer = LineWriter::new(buffer);
    writer.push(b"IPC DENIED=");
    writer.value(syscall::ipc_denials());
    writer.end_line();
    writer.push(b"USERS EXITS=");
    writer.value(scheduler::user_exits());
    writer.push(b" FAULTS=");
    writer.value(scheduler::user_faults());
    writer.end_line();
    writer.push(b"PORTAL GRANTS=");
    writer.value(portal::grant_count());
    writer.push(b" DENIED=");
    writer.value(portal::denials());
    writer.end_line();
    writer.push(b"INPUT DROPPED=");
    writer.value(crate::input::dropped() as usize);
    writer.end_line();
    writer.push(b"COMPOSITOR SOFTWARE");
    writer.end_line();
    writer.push(b"FRAMEBUFFER LINEAR");
    writer.end_line();
    writer.length()
}
