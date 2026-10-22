// SPDX-License-Identifier: Apache-2.0
//! Full-screen crash report for fatal kernel errors.
//!
//! Every fatal path (a panic, an unhandled kernel exception including a
//! double fault, and every `AETHER_BOOT_FAILED` halt) ends in
//! [`crate::arch::x86_64::halt`], which calls [`fatal`]. A machine without a
//! saved serial log would otherwise show a frozen desktop, so [`fatal`] paints
//! the reason, the build, the faulting CPU state, and the tail of the console
//! log straight onto the framebuffer.
//!
//! This code runs with the kernel in an unknown state, so it allocates
//! nothing, takes no locks, and does not use the graphics service: it keeps
//! its own copy of the framebuffer geometry and writes pixels directly. A
//! fault or panic while the screen is painted halts without painting again.

use core::cell::UnsafeCell;
use core::fmt::{self, Write};
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use aether_abi::{FramebufferInfo, PixelFormat};

use crate::arch::x86_64::idt::InterruptFrame;
use crate::arch::x86_64::serial;

/// Bytes of console output kept for the report.
const LOG_SIZE: usize = 8 * 1024;
/// Longest line the report formats; longer lines are cut.
const LINE_SIZE: usize = 160;

const BACKGROUND: u32 = 0x0f_3c_8c;
const TEXT: u32 = 0xff_ff_ff;
const DIM_TEXT: u32 = 0xb4_c6_e6;
const LABEL: u32 = 0xff_d8_6a;

/// Interior-mutable static owned by the fatal path.
///
/// Writers are the console (one CPU; an interrupt can interleave a byte,
/// which only garbles the report) and the fatal path, which runs once with
/// interrupts off.
struct Global<T>(UnsafeCell<T>);

// SAFETY: See the type documentation; the kernel runs on one CPU.
unsafe impl<T> Sync for Global<T> {}

static LOG: Global<[u8; LOG_SIZE]> = Global(UnsafeCell::new([0; LOG_SIZE]));
/// Total bytes ever recorded; the ring position is this modulo [`LOG_SIZE`].
static LOG_HEAD: AtomicUsize = AtomicUsize::new(0);
/// The log in order, copied once the fatal path starts.
static SNAPSHOT: Global<[u8; LOG_SIZE]> = Global(UnsafeCell::new([0; LOG_SIZE]));

/// Framebuffer address, or zero when there is no usable display.
static FB_BASE: AtomicU64 = AtomicU64::new(0);
static FB_WIDTH: AtomicU32 = AtomicU32::new(0);
static FB_HEIGHT: AtomicU32 = AtomicU32::new(0);
static FB_STRIDE: AtomicU32 = AtomicU32::new(0);
static FB_FORMAT: AtomicU32 = AtomicU32::new(0);

/// CPU state of the kernel exception that led to the halt.
#[derive(Clone, Copy)]
struct ExceptionState {
    frame: InterruptFrame,
    cr2: u64,
    cr3: u64,
    name: &'static str,
}

static EXCEPTION: Global<Option<ExceptionState>> = Global(UnsafeCell::new(None));

static PANIC_MESSAGE: Global<LineBuffer> = Global(UnsafeCell::new(LineBuffer::new()));
static PANIC_LOCATION: Global<LineBuffer> = Global(UnsafeCell::new(LineBuffer::new()));
static PANICKED: AtomicBool = AtomicBool::new(false);

/// Number of times [`fatal`] was entered; only the first paints.
static ENTERED: AtomicUsize = AtomicUsize::new(0);

/// Records one console byte for the report.
pub fn record_byte(byte: u8) {
    if byte == b'\r' {
        return;
    }
    let at = LOG_HEAD.fetch_add(1, Ordering::Relaxed);
    // SAFETY: The index is in bounds; see [`Global`] for concurrent access.
    unsafe { (*LOG.0.get())[at % LOG_SIZE] = byte };
}

/// Remembers the display so a later fatal error can paint on it.
///
/// Called with the loader's description before the kernel's page tables are
/// live (the firmware identity map covers the framebuffer) and again once the
/// kernel map is known. Unusable descriptions leave the report on serial only.
pub fn set_framebuffer(info: &FramebufferInfo) {
    let usable = info.is_present()
        && info.validate().is_ok()
        && PixelFormat::from_raw(info.format).is_some_and(PixelFormat::is_linear);
    if !usable {
        clear_framebuffer();
        return;
    }
    FB_BASE.store(0, Ordering::Release);
    FB_WIDTH.store(info.width, Ordering::Relaxed);
    FB_HEIGHT.store(info.height, Ordering::Relaxed);
    FB_STRIDE.store(info.stride, Ordering::Relaxed);
    FB_FORMAT.store(info.format, Ordering::Relaxed);
    FB_BASE.store(info.base, Ordering::Release);
}

/// Forgets the display, for example when the kernel map leaves it unmapped.
pub fn clear_framebuffer() {
    FB_BASE.store(0, Ordering::Release);
}

/// Records the CPU state of a fatal kernel exception.
pub fn record_exception(frame: &InterruptFrame, cr2: u64, name: &'static str) {
    let cr3: u64;
    // SAFETY: Reading CR3 has no side effects in ring 0.
    unsafe {
        core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack, preserves_flags))
    };
    // SAFETY: See [`Global`]; the exception path runs with interrupts off.
    unsafe {
        *EXCEPTION.0.get() = Some(ExceptionState {
            frame: *frame,
            cr2,
            cr3,
            name,
        });
    }
}

/// Records the panic message and location for the report.
///
/// Returns `false` for a panic raised while an earlier panic was being
/// reported; the caller then skips its own formatting and goes straight to
/// [`fatal`], because formatting is what may have panicked.
pub fn record_panic(info: &core::panic::PanicInfo<'_>) -> bool {
    if PANICKED.swap(true, Ordering::AcqRel) {
        return false;
    }
    // SAFETY: Only the first panic reaches this point.
    let (message, location) =
        unsafe { (&mut *PANIC_MESSAGE.0.get(), &mut *PANIC_LOCATION.0.get()) };
    let _ = write!(message, "{}", info.message());
    if let Some(place) = info.location() {
        let _ = write!(
            location,
            "{}:{}:{}",
            place.file(),
            place.line(),
            place.column()
        );
    }
    true
}

/// Stops the machine after a fatal error and shows the crash report.
pub fn fatal() -> ! {
    // SAFETY: Masking interrupts is always allowed in ring 0.
    unsafe { core::arch::asm!("cli", options(nomem, nostack)) };
    crate::arch::x86_64::usercopy::clear_ac();
    match ENTERED.fetch_add(1, Ordering::AcqRel) {
        0 => {}
        1 => {
            serial::write_str("AETHER_CRASH_SCREEN nested fault while reporting; halted\n");
            halt_forever()
        }
        _ => halt_forever(),
    }

    let log = snapshot();
    let mut reason = LineBuffer::new();
    find_reason(log, &mut reason);
    let shown = paint(log, reason.as_str());
    let _ = writeln!(
        serial::Serial,
        "AETHER_CRASH_SCREEN shown={} reason={}",
        u8::from(shown),
        reason.as_str()
    );
    halt_forever()
}

fn halt_forever() -> ! {
    loop {
        // SAFETY: Nothing else will run; an NMI only wakes the loop.
        unsafe { core::arch::asm!("cli", "hlt", options(nomem, nostack)) };
    }
}

/// Copies the console ring into [`SNAPSHOT`] in order, starting at a line.
fn snapshot() -> &'static [u8] {
    let head = LOG_HEAD.load(Ordering::Acquire);
    let len = head.min(LOG_SIZE);
    // SAFETY: The fatal path runs once with interrupts off; nothing else
    // writes the snapshot, and console bytes written after this are ignored.
    let (log, out) = unsafe { (&*LOG.0.get(), &mut *SNAPSHOT.0.get()) };
    for (index, byte) in out[..len].iter_mut().enumerate() {
        *byte = log[(head - len + index) % LOG_SIZE];
    }
    let mut start = 0;
    if head > LOG_SIZE
        && let Some(newline) = out[..len].iter().position(|&byte| byte == b'\n')
    {
        start = newline + 1;
    }
    &out[start..len]
}

/// Picks the one-line reason: the panic message, else the recorded
/// exception, else the newest `AETHER_BOOT_FAILED reason=` line, else the
/// newest exception line.
fn find_reason(log: &[u8], out: &mut LineBuffer) {
    if PANICKED.load(Ordering::Acquire) {
        // SAFETY: Written once by `record_panic` before `fatal` ran.
        let message = unsafe { (*PANIC_MESSAGE.0.get()).as_str() };
        let _ = write!(out, "kernel panic: {message}");
        return;
    }
    // SAFETY: Written by `record_exception` before `fatal` ran.
    if let Some(state) = unsafe { *EXCEPTION.0.get() } {
        let _ = write!(
            out,
            "unhandled kernel exception: {} at rip={:#x}",
            state.name, state.frame.rip
        );
        return;
    }
    const BOOT_FAILED: &[u8] = b"AETHER_BOOT_FAILED reason=";
    const EXCEPTION_LINE: &[u8] = b"KERNEL EXCEPTION: ";
    for marker in [BOOT_FAILED, EXCEPTION_LINE] {
        if let Some(line) = log
            .split(|&byte| byte == b'\n')
            .rev()
            .find_map(|line| find(line, marker).map(|at| &line[at + marker.len()..]))
        {
            out.push_bytes(line);
            return;
        }
    }
    let _ = out.write_str("unknown (see the log below)");
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Fixed-size text buffer; text past the end is dropped.
struct LineBuffer {
    bytes: [u8; LINE_SIZE],
    len: usize,
}

impl LineBuffer {
    const fn new() -> Self {
        Self {
            bytes: [0; LINE_SIZE],
            len: 0,
        }
    }

    fn push_bytes(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if self.len == LINE_SIZE {
                return;
            }
            self.bytes[self.len] = if byte.is_ascii_graphic() || byte == b' ' {
                byte
            } else {
                b'?'
            };
            self.len += 1;
        }
    }

    fn as_str(&self) -> &str {
        // Only printable ASCII is ever stored.
        core::str::from_utf8(&self.bytes[..self.len]).unwrap_or("")
    }

    fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    fn clear(&mut self) {
        self.len = 0;
    }
}

impl Write for LineBuffer {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        self.push_bytes(value.as_bytes());
        Ok(())
    }
}

/// Direct framebuffer writer used only by the fatal path.
struct Screen {
    base: *mut u32,
    width: i32,
    height: i32,
    stride: usize,
    format: PixelFormat,
}

impl Screen {
    fn current() -> Option<Self> {
        let base = FB_BASE.load(Ordering::Acquire);
        if base == 0 {
            return None;
        }
        Some(Self {
            base: base as *mut u32,
            width: FB_WIDTH.load(Ordering::Relaxed) as i32,
            height: FB_HEIGHT.load(Ordering::Relaxed) as i32,
            stride: FB_STRIDE.load(Ordering::Relaxed) as usize,
            format: PixelFormat::from_raw(FB_FORMAT.load(Ordering::Relaxed))?,
        })
    }

    fn fill(&self, x: i32, y: i32, width: i32, height: i32, rgb: u32) {
        let pixel = self.format.encode_rgb(rgb).unwrap_or(0);
        let left = x.max(0);
        let right = (x + width).min(self.width);
        for row in y.max(0)..(y + height).min(self.height) {
            for column in left..right {
                // SAFETY: `set_framebuffer` accepted a validated description,
                // so every in-bounds pixel lies inside the mapped buffer.
                unsafe {
                    self.base
                        .add(row as usize * self.stride + column as usize)
                        .write_volatile(pixel);
                }
            }
        }
    }
}

/// Text layout over the [`Screen`] in the kernel's 5x7 font.
struct Painter<'a> {
    screen: &'a Screen,
    scale: i32,
    left: i32,
    columns: usize,
    y: i32,
    bottom: i32,
}

impl Painter<'_> {
    fn line_height(&self, scale: i32) -> i32 {
        10 * scale
    }

    fn rows_left(&self) -> usize {
        ((self.bottom - self.y).max(0) / self.line_height(self.scale)) as usize
    }

    fn text_at(&self, x: i32, y: i32, text: &[u8], rgb: u32, scale: i32) -> i32 {
        let mut cursor = x;
        for &byte in text {
            let character = if byte.is_ascii_graphic() {
                byte as char
            } else {
                ' '
            };
            let rows = crate::graphics::glyph(character.to_ascii_uppercase());
            for (row, bits) in rows.iter().enumerate() {
                for column in 0..5 {
                    if bits & (1 << (4 - column)) != 0 {
                        self.screen.fill(
                            cursor + column * scale,
                            y + row as i32 * scale,
                            scale,
                            scale,
                            rgb,
                        );
                    }
                }
            }
            cursor += 6 * scale;
        }
        cursor
    }

    /// Draws one row at `scale`, cut to the screen width.
    fn line(&mut self, text: &[u8], rgb: u32, scale: i32) {
        if self.y + self.line_height(scale) > self.bottom {
            return;
        }
        let columns = (self.columns as i32 * self.scale / scale) as usize;
        self.text_at(
            self.left,
            self.y,
            &text[..text.len().min(columns)],
            rgb,
            scale,
        );
        self.y += self.line_height(scale);
    }

    /// Draws `label` then `value`, wrapping the value under itself.
    fn field(&mut self, label: &[u8], value: &[u8], max_rows: usize) {
        const LABEL_COLUMNS: usize = 11;
        let width = self.columns.saturating_sub(LABEL_COLUMNS).max(1);
        let mut rest = value;
        let mut first = true;
        for _ in 0..max_rows.max(1) {
            if self.rows_left() == 0 || (!first && rest.is_empty()) {
                return;
            }
            if first {
                self.text_at(self.left, self.y, label, LABEL, self.scale);
            }
            let take = rest.len().min(width);
            let x = self.left + LABEL_COLUMNS as i32 * 6 * self.scale;
            self.text_at(x, self.y, &rest[..take], TEXT, self.scale);
            rest = &rest[take..];
            self.y += self.line_height(self.scale);
            first = false;
        }
    }

    fn gap(&mut self) {
        self.y += self.line_height(self.scale) / 2;
    }
}

/// Paints the report. Returns `false` when there is no display.
fn paint(log: &[u8], reason: &str) -> bool {
    let Some(screen) = Screen::current() else {
        return false;
    };
    screen.fill(0, 0, screen.width, screen.height, BACKGROUND);

    // Scale the 5x7 font so the report reads like a console at any mode:
    // 1 below 1024x768, 2 at 1024x768, 3 at 1920x1080.
    let scale = (screen.width / 600).min(screen.height / 380).clamp(1, 4);
    let margin = (screen.width / 20).max(8);
    let columns = ((screen.width - 2 * margin) / (6 * scale)).max(1) as usize;
    let mut painter = Painter {
        screen: &screen,
        scale,
        left: margin,
        columns,
        y: (screen.height / 16).max(8),
        bottom: screen.height - margin / 2,
    };

    painter.line(b":(", TEXT, scale * 4);
    painter.gap();
    painter.line(b"Aether ran into a problem and stopped.", TEXT, scale + 1);
    painter.gap();
    painter.line(
        b"The system was halted to protect your data. Restart the machine to continue.",
        DIM_TEXT,
        scale,
    );
    painter.line(
        b"If you report this, include a photo of this screen.",
        DIM_TEXT,
        scale,
    );
    painter.gap();

    let mut line = LineBuffer::new();
    painter.field(b"REASON", reason.as_bytes(), 3);
    if PANICKED.load(Ordering::Acquire) {
        // SAFETY: Written once by `record_panic` before `fatal` ran.
        let location = unsafe { (*PANIC_LOCATION.0.get()).as_str() };
        if !location.is_empty() {
            painter.field(b"WHERE", location.as_bytes(), 2);
        }
    }
    let _ = write!(
        line,
        "{}  commit {}",
        env!("CARGO_PKG_VERSION"),
        option_env!("AETHER_GIT_COMMIT").unwrap_or("unknown")
    );
    painter.field(b"VERSION", line.as_bytes(), 1);

    // SAFETY: Written by `record_exception` before `fatal` ran.
    if let Some(state) = unsafe { *EXCEPTION.0.get() } {
        let frame = &state.frame;
        line.clear();
        let _ = write!(
            line,
            "{} (vector {}) error={:#x}",
            state.name, frame.vector, frame.error
        );
        painter.field(b"EXCEPTION", line.as_bytes(), 1);
        painter.gap();
        let registers: [(&str, u64); 22] = [
            ("RIP", frame.rip),
            ("CR2", state.cr2),
            ("CR3", state.cr3),
            ("RSP", frame.rsp),
            ("RFLAGS", frame.rflags),
            ("CS", frame.cs),
            ("SS", frame.ss),
            ("RAX", frame.rax),
            ("RBX", frame.rbx),
            ("RCX", frame.rcx),
            ("RDX", frame.rdx),
            ("RSI", frame.rsi),
            ("RDI", frame.rdi),
            ("RBP", frame.rbp),
            ("R8", frame.r8),
            ("R9", frame.r9),
            ("R10", frame.r10),
            ("R11", frame.r11),
            ("R12", frame.r12),
            ("R13", frame.r13),
            ("R14", frame.r14),
            ("R15", frame.r15),
        ];
        // "RFLAGS=0x0000000000000000  " is 27 columns.
        let per_row = (columns / 27).clamp(1, 4);
        for chunk in registers.chunks(per_row) {
            line.clear();
            for (name, value) in chunk {
                let _ = write!(line, "{name:>6}={value:#018x}  ");
            }
            painter.line(line.as_bytes(), TEXT, scale);
        }
    }

    painter.gap();
    painter.line(b"Last console output:", LABEL, scale);
    let rows = painter.rows_left();
    let lines = log
        .split(|&byte| byte == b'\n')
        .filter(|line| !line.is_empty());
    let total = lines.clone().count();
    for text in lines.skip(total.saturating_sub(rows)) {
        painter.line(text, DIM_TEXT, scale);
    }
    true
}
