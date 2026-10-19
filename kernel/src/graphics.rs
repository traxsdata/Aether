// SPDX-License-Identifier: Apache-2.0
//! Linear framebuffer graphics service.
//!
//! The loader hands over a linear, 32-bit GOP framebuffer. The kernel maps it
//! cache-disabled during paging setup and this module owns all pixel writes.
//! There is no GPU command stream: the first desktop paints a software
//! compositor directly into the framebuffer.

use crate::serial_line;
use aether_abi::{FramebufferInfo, PixelFormat};
use core::cell::UnsafeCell;

struct Global<T>(UnsafeCell<T>);

// SAFETY: The framebuffer is only accessed from the single CPU. The idle task
// and shell repaint synchronously; input handlers only enqueue keys.
unsafe impl<T> Sync for Global<T> {}

struct Framebuffer {
    base: *mut u8,
    width: u32,
    height: u32,
    stride: u32,
    format: PixelFormat,
}

unsafe impl Send for Framebuffer {}

static FRAMEBUFFER: Global<Option<Framebuffer>> = Global(UnsafeCell::new(None));

/// Colours in `0x00RRGGBB` form.
pub mod color {
    /// Desktop background.
    pub const BACKGROUND: u32 = 0x0b1220;
    /// Window body.
    pub const WINDOW: u32 = 0x16213a;
    /// Raised window title bar.
    pub const TITLE: u32 = 0x223255;
    /// Top bar and taskbar.
    pub const BAR: u32 = 0x101a30;
    /// Muted text.
    pub const MUTED: u32 = 0x8fa3c8;
    /// Primary text.
    pub const TEXT: u32 = 0xe8eef9;
    /// Accent used for focus and the security indicator.
    pub const ACCENT: u32 = 0x4fd1c5;
    /// Warning or denied state.
    pub const WARNING: u32 = 0xf6ad55;
    /// Window shadow.
    pub const SHADOW: u32 = 0x070c16;
    /// Divider line.
    pub const DIVIDER: u32 = 0x2d3d63;
}

/// Graphics initialization failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphicsError {
    /// The framebuffer descriptor passed ABI validation but is unusable.
    UnsupportedFormat,
}

impl Framebuffer {
    fn pack(&self, rgb: u32) -> u32 {
        match self.format {
            PixelFormat::RedGreenBlueReserved => {
                ((rgb & 0x0000_00ff) << 16) | (rgb & 0x0000_ff00) | ((rgb >> 16) & 0x0000_00ff)
            }
            _ => rgb,
        }
    }

    fn pixel(&mut self, x: i32, y: i32, rgb: u32) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        let offset = (y as usize * self.stride as usize + x as usize) * 4;
        // SAFETY: The ABI validated that the framebuffer covers
        // `stride * height * 4` bytes, and the bounds were checked above.
        unsafe {
            let pointer = self.base.add(offset) as *mut u32;
            pointer.write_volatile(self.pack(rgb));
        }
    }

    fn rect(&mut self, x: i32, y: i32, width: i32, height: i32, rgb: u32) {
        for row in 0..height {
            for column in 0..width {
                self.pixel(x + column, y + row, rgb);
            }
        }
    }
}

/// Initializes the graphics service. Returns `false` for a headless machine.
pub fn init(info: &FramebufferInfo, mapped: bool) -> Result<bool, GraphicsError> {
    if !info.is_present() || !mapped {
        return Ok(false);
    }
    let Some(format) = PixelFormat::from_raw(info.format) else {
        return Err(GraphicsError::UnsupportedFormat);
    };
    if !format.is_linear() {
        return Err(GraphicsError::UnsupportedFormat);
    }
    // SAFETY: The framebuffer is mapped cache-disabled by the paging layer and
    // the ABI validated its length.
    unsafe {
        *FRAMEBUFFER.0.get() = Some(Framebuffer {
            base: info.base as *mut u8,
            width: info.width,
            height: info.height,
            stride: info.stride,
            format,
        });
    }
    serial_line!(
        "graphics: framebuffer {}x{} format={:?}",
        info.width,
        info.height,
        format
    );
    Ok(true)
}

/// Returns `true` if a linear framebuffer is available.
#[must_use]
pub fn is_ready() -> bool {
    // SAFETY: The slot is written once during boot and then read-only.
    unsafe { (*FRAMEBUFFER.0.get()).is_some() }
}

/// Returns the visible width.
#[must_use]
pub fn width() -> u32 {
    // SAFETY: Read-only access to the boot-time descriptor.
    unsafe { (*FRAMEBUFFER.0.get()).as_ref().map_or(0, |fb| fb.width) }
}

/// Returns the visible height.
#[must_use]
pub fn height() -> u32 {
    // SAFETY: Read-only access to the boot-time descriptor.
    unsafe { (*FRAMEBUFFER.0.get()).as_ref().map_or(0, |fb| fb.height) }
}

fn with_framebuffer<R>(f: impl FnOnce(&mut Framebuffer) -> R) -> Option<R> {
    // SAFETY: All callers run on the single CPU and do not hold another
    // mutable reference across this call.
    unsafe { (*FRAMEBUFFER.0.get()).as_mut().map(f) }
}

/// Fills the whole screen.
pub fn clear(rgb: u32) {
    let _ = with_framebuffer(|fb| {
        let width = fb.width as i32;
        let height = fb.height as i32;
        fb.rect(0, 0, width, height, rgb);
    });
}

/// Fills a rectangle, clipped to the screen.
pub fn fill_rect(x: i32, y: i32, width: i32, height: i32, rgb: u32) {
    let _ = with_framebuffer(|fb| fb.rect(x, y, width, height, rgb));
}

/// Draws a one-pixel rectangle outline.
pub fn outline(x: i32, y: i32, width: i32, height: i32, rgb: u32) {
    let _ = with_framebuffer(|fb| {
        for column in 0..width {
            fb.pixel(x + column, y, rgb);
            fb.pixel(x + column, y + height - 1, rgb);
        }
        for row in 0..height {
            fb.pixel(x, y + row, rgb);
            fb.pixel(x + width - 1, y + row, rgb);
        }
    });
}

/// Returns the 5x7 glyph rows for `character`. Lowercase input is folded to
/// uppercase by [`draw_text`].
fn glyph(character: char) -> [u8; 7] {
    match character {
        'A' => [0x0e, 0x11, 0x11, 0x1f, 0x11, 0x11, 0x11],
        'B' => [0x1e, 0x11, 0x11, 0x1e, 0x11, 0x11, 0x1e],
        'C' => [0x0e, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0e],
        'D' => [0x1e, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1e],
        'E' => [0x1f, 0x10, 0x10, 0x1e, 0x10, 0x10, 0x1f],
        'F' => [0x1f, 0x10, 0x10, 0x1e, 0x10, 0x10, 0x10],
        'G' => [0x0e, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0f],
        'H' => [0x11, 0x11, 0x11, 0x1f, 0x11, 0x11, 0x11],
        'I' => [0x0e, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0e],
        'J' => [0x07, 0x02, 0x02, 0x02, 0x02, 0x12, 0x0c],
        'K' => [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11],
        'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1f],
        'M' => [0x11, 0x1b, 0x15, 0x15, 0x11, 0x11, 0x11],
        'N' => [0x11, 0x19, 0x15, 0x13, 0x11, 0x11, 0x11],
        'O' => [0x0e, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0e],
        'P' => [0x1e, 0x11, 0x11, 0x1e, 0x10, 0x10, 0x10],
        'Q' => [0x0e, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0d],
        'R' => [0x1e, 0x11, 0x11, 0x1e, 0x14, 0x12, 0x11],
        'S' => [0x0f, 0x10, 0x10, 0x0e, 0x01, 0x01, 0x1e],
        'T' => [0x1f, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0e],
        'V' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x0a, 0x04],
        'W' => [0x11, 0x11, 0x11, 0x15, 0x15, 0x15, 0x0a],
        'X' => [0x11, 0x11, 0x0a, 0x04, 0x0a, 0x11, 0x11],
        'Y' => [0x11, 0x11, 0x0a, 0x04, 0x04, 0x04, 0x04],
        'Z' => [0x1f, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1f],
        '0' => [0x0e, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0e],
        '1' => [0x04, 0x0c, 0x04, 0x04, 0x04, 0x04, 0x0e],
        '2' => [0x0e, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1f],
        '3' => [0x1e, 0x01, 0x01, 0x0e, 0x01, 0x01, 0x1e],
        '4' => [0x02, 0x06, 0x0a, 0x12, 0x1f, 0x02, 0x02],
        '5' => [0x1f, 0x10, 0x1e, 0x01, 0x01, 0x11, 0x0e],
        '6' => [0x06, 0x08, 0x10, 0x1e, 0x11, 0x11, 0x0e],
        '7' => [0x1f, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0e, 0x11, 0x11, 0x0e, 0x11, 0x11, 0x0e],
        '9' => [0x0e, 0x11, 0x11, 0x0f, 0x01, 0x02, 0x0c],
        ':' => [0x00, 0x04, 0x04, 0x00, 0x04, 0x04, 0x00],
        '.' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x04],
        '/' => [0x01, 0x02, 0x02, 0x04, 0x08, 0x08, 0x10],
        '-' => [0x00, 0x00, 0x00, 0x1f, 0x00, 0x00, 0x00],
        '_' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1f],
        '%' => [0x11, 0x12, 0x04, 0x04, 0x08, 0x09, 0x11],
        '(' => [0x02, 0x04, 0x08, 0x08, 0x08, 0x04, 0x02],
        ')' => [0x08, 0x04, 0x02, 0x02, 0x02, 0x04, 0x08],
        '[' => [0x0e, 0x08, 0x08, 0x08, 0x08, 0x08, 0x0e],
        ']' => [0x0e, 0x02, 0x02, 0x02, 0x02, 0x02, 0x0e],
        '>' => [0x08, 0x04, 0x02, 0x01, 0x02, 0x04, 0x08],
        '<' => [0x02, 0x04, 0x08, 0x10, 0x08, 0x04, 0x02],
        '=' => [0x00, 0x00, 0x1f, 0x00, 0x1f, 0x00, 0x00],
        '+' => [0x00, 0x04, 0x04, 0x1f, 0x04, 0x04, 0x00],
        ',' => [0x00, 0x00, 0x00, 0x00, 0x04, 0x04, 0x08],
        '!' => [0x04, 0x04, 0x04, 0x04, 0x04, 0x00, 0x04],
        '?' => [0x0e, 0x11, 0x01, 0x02, 0x04, 0x00, 0x04],
        '*' => [0x00, 0x15, 0x0e, 0x1f, 0x0e, 0x15, 0x00],
        '#' => [0x0a, 0x0a, 0x1f, 0x0a, 0x1f, 0x0a, 0x0a],
        '\'' => [0x04, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00],
        '"' => [0x0a, 0x0a, 0x00, 0x00, 0x00, 0x00, 0x00],
        '@' => [0x0e, 0x11, 0x17, 0x15, 0x17, 0x10, 0x0e],
        _ => [0x00; 7],
    }
}

/// Draws one character and returns the horizontal advance.
pub fn draw_char(x: i32, y: i32, character: char, rgb: u32, scale: i32) -> i32 {
    let uppercase = character.to_ascii_uppercase();
    let rows = glyph(uppercase);
    let scale = scale.max(1);
    let _ = with_framebuffer(|fb| {
        for (row, bits) in rows.iter().enumerate() {
            for column in 0..5 {
                if bits & (1 << (4 - column)) != 0 {
                    fb.rect(
                        x + column * scale,
                        y + row as i32 * scale,
                        scale,
                        scale,
                        rgb,
                    );
                }
            }
        }
    });
    6 * scale
}

/// Draws a single-line string and returns the horizontal advance.
pub fn draw_text(x: i32, y: i32, text: &str, rgb: u32, scale: i32) -> i32 {
    let mut cursor = x;
    for character in text.chars() {
        cursor += draw_char(cursor, y, character, rgb, scale);
    }
    cursor - x
}

/// Returns a deterministic checksum of sampled framebuffer pixels.
///
/// This is the evidence the boot test uses to prove the compositor actually
/// wrote pixels, without requiring a screenshot.
#[must_use]
pub fn checksum() -> u32 {
    let Some(hash) = with_framebuffer(|fb| {
        let mut hash = 0x811c_9dc5u32;
        let step_x = (fb.width / 16).max(1);
        let step_y = (fb.height / 12).max(1);
        let mut y = 0;
        while y < fb.height {
            let mut x = 0;
            while x < fb.width {
                let offset = (y as usize * fb.stride as usize + x as usize) * 4;
                // SAFETY: Bounds are inside the validated framebuffer.
                let value = unsafe { (fb.base.add(offset) as *const u32).read_volatile() };
                hash ^= value;
                hash = hash.wrapping_mul(0x0100_0193);
                x += step_x;
            }
            y += step_y;
        }
        hash
    }) else {
        return 0;
    };
    hash
}
