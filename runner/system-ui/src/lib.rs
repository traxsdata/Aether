//! system-ui - rust system apps draw with the OS's own graphics.
//!
//! This is a thin Rust binding to `common/gfx.c`: anti-aliased rectangles,
//! lines and circles, and text in Facet, æther's own font. It deliberately
//! does not pull in egui, epaint, third-party fonts or any other external
//! crate. The window and input side uses `aether-host`.

use std::ffi::CString;
use std::marker::PhantomData;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }
    pub fn contains(self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }
}

#[repr(C)]
struct Surface {
    px: *mut u32,
    w: i32,
    h: i32,
    pitch: i32,
    swap: i32,
    clip: Rect,
}

#[repr(i32)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Font {
    Small = 0,
    Medium = 1,
    Large = 2,
}

extern "C" {
    fn aether_gfx_init(s: *mut Surface, px: *mut u32, w: i32, h: i32, pitch: i32);
    fn aether_gfx_fill(s: *mut Surface, x: i32, y: i32, w: i32, h: i32, c: u32);
    fn aether_gfx_blend(s: *mut Surface, x: i32, y: i32, w: i32, h: i32, c: u32, a: u32);
    fn aether_gfx_round_rect(s: *mut Surface, x: i32, y: i32, w: i32, h: i32, r: i32, c: u32, a: u32);
    fn aether_gfx_round_outline(s: *mut Surface, x: i32, y: i32, w: i32, h: i32, r: i32, c: u32, a: u32);
    fn aether_gfx_circle(s: *mut Surface, cx: i32, cy: i32, r: i32, c: u32, a: u32);
    fn aether_gfx_image(s: *mut Surface, x: i32, y: i32, w: i32, h: i32, rgba: *const u8, iw: i32, ih: i32);
    fn aether_gfx_text(s: *mut Surface, x: i32, y: i32, text: *const i8, c: u32, font: i32) -> i32;
    fn aether_gfx_text_width(text: *const i8, font: i32) -> i32;
    fn aether_gfx_font_h(font: i32) -> i32;
    fn aether_gfx_font_w(font: i32) -> i32;
}

fn cstr(s: &str) -> CString {
    let n = s.find('\0').unwrap_or(s.len());
    CString::new(&s.as_bytes()[..n]).unwrap_or_default()
}

/// A drawing surface over one window framebuffer.
pub struct Canvas<'a> {
    s: Surface,
    width: u32,
    height: u32,
    _marker: PhantomData<&'a mut [u32]>,
}

impl<'a> Canvas<'a> {
    pub fn new(pixels: &'a mut [u32], width: u32, height: u32, pitch: u32) -> Self {
        assert!(width <= pitch, "system-ui: width is larger than the row pitch");
        let need = (pitch as usize).checked_mul(height as usize);
        assert!(
            need.is_some_and(|n| n <= pixels.len()),
            "system-ui: the pixel slice is smaller than pitch * height"
        );
        let mut s = Surface {
            px: pixels.as_mut_ptr(),
            w: width as i32,
            h: height as i32,
            pitch: pitch as i32,
            swap: 0,
            clip: Rect::new(0, 0, width as i32, height as i32),
        };
        unsafe { aether_gfx_init(&mut s, pixels.as_mut_ptr(), width as i32, height as i32, pitch as i32) };
        Self { s, width, height, _marker: PhantomData }
    }

    pub fn width(&self) -> u32 { self.width }
    pub fn height(&self) -> u32 { self.height }

    pub fn fill(&mut self, x: i32, y: i32, w: i32, h: i32, color: u32) {
        unsafe { aether_gfx_fill(&mut self.s, x, y, w, h, color) }
    }

    pub fn clear(&mut self, color: u32) {
        self.fill(0, 0, self.width as i32, self.height as i32, color);
    }

    pub fn blend(&mut self, x: i32, y: i32, w: i32, h: i32, color: u32, alpha: u8) {
        unsafe { aether_gfx_blend(&mut self.s, x, y, w, h, color, alpha as u32) }
    }

    pub fn line(&mut self, x: i32, y: i32, w: i32, h: i32, color: u32) {
        self.fill(x, y, w, h, color);
    }

    pub fn round_rect(&mut self, x: i32, y: i32, w: i32, h: i32, radius: i32, color: u32) {
        unsafe { aether_gfx_round_rect(&mut self.s, x, y, w, h, radius, color, 255) }
    }

    pub fn round_outline(&mut self, x: i32, y: i32, w: i32, h: i32, radius: i32, color: u32, alpha: u8) {
        unsafe { aether_gfx_round_outline(&mut self.s, x, y, w, h, radius, color, alpha as u32) }
    }

    pub fn circle(&mut self, cx: i32, cy: i32, radius: i32, color: u32) {
        unsafe { aether_gfx_circle(&mut self.s, cx, cy, radius, color, 255) }
    }

    /// Draws an RGBA8888 image, scaled to fill the rectangle. A short slice
    /// is ignored rather than handed to C: the C function trusts its caller.
    pub fn image(&mut self, x: i32, y: i32, w: i32, h: i32, rgba: &[u8], iw: u32, ih: u32) {
        if iw == 0 || ih == 0 || iw > i32::MAX as u32 || ih > i32::MAX as u32 {
            return;
        }
        let Some(size) = (iw as usize).checked_mul(ih as usize).and_then(|n| n.checked_mul(4)) else {
            return;
        };
        if rgba.len() < size {
            return;
        }
        unsafe { aether_gfx_image(&mut self.s, x, y, w, h, rgba.as_ptr(), iw as i32, ih as i32) }
    }

    /// Draws text with its top-left at (x, y). Returns the x after the text.
    pub fn text(&mut self, x: i32, y: i32, text: &str, color: u32, font: Font) -> i32 {
        let c = cstr(text);
        unsafe { aether_gfx_text(&mut self.s, x, y, c.as_ptr(), color, font as i32) }
    }

    /// Simple text in the common colour; returns the next x.
    pub fn label(&mut self, x: i32, y: i32, text: &str, color: u32, font: Font) -> i32 {
        self.text(x, y, text, color, font)
    }
}

pub fn text_width(text: &str, font: Font) -> i32 {
    let c = cstr(text);
    unsafe { aether_gfx_text_width(c.as_ptr(), font as i32) }
}

pub fn font_h(font: Font) -> i32 {
    unsafe { aether_gfx_font_h(font as i32) }
}

pub fn font_w(font: Font) -> i32 {
    unsafe { aether_gfx_font_w(font as i32) }
}

/// Truncates `s` so it fits in `max` pixels, with an ellipsis if cut.
pub fn ellipsize(s: &str, font: Font, max: i32) -> String {
    if text_width(s, font) <= max {
        return s.to_string();
    }
    let dots = "...";
    let dw = text_width(dots, font);
    let mut out = String::new();
    for ch in s.chars() {
        let mut next = out.clone();
        next.push(ch);
        if text_width(&next, font) + dw > max {
            break;
        }
        out = next;
    }
    out.push_str(dots);
    out
}

use aether_host::{Backend, Event};

pub trait App {
    fn draw(&mut self, canvas: &mut Canvas<'_>, width: u32, height: u32);
    fn event(&mut self, event: &Event, backend: &mut dyn Backend);
    fn update(&mut self, _backend: &mut dyn Backend) -> bool {
        false
    }
}

/// Opens a window, draws the app, and waits for input. This is the same
/// software loop the native C apps use, without egui or a GPU.
pub fn run<A: App>(app: &mut A, width: u32, height: u32, title: &str) -> Result<(), String> {
    let mut backend = aether_host::open();
    let (win, _w, _h) = backend
        .create_window(width, height, title)
        .map_err(|e| format!("can't create window: {e}"))?;
    let mut dirty = true;
    loop {
        if dirty {
            if let Some(fb) = backend.framebuffer(win) {
                let mut canvas = Canvas::new(fb.pixels, fb.width, fb.height, fb.pitch);
                app.draw(&mut canvas, fb.width, fb.height);
            }
            if backend.present(win).is_err() {
                break;
            }
            dirty = false;
        }
        if app.update(&mut *backend) {
            dirty = true;
        }
        match backend.wait_event(Some(200)) {
            Ok(Some(Event::Close)) => break,
            Ok(Some(Event::Resize { .. })) => {
                dirty = true;
            }
            Ok(Some(ev)) => {
                app.event(&ev, &mut *backend);
                dirty = true;
            }
            Ok(None) => {}
            Err(_) => break,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_image_slice_is_ignored() {
        let mut px = vec![0u32; 64 * 64];
        let mut c = Canvas::new(&mut px, 64, 64, 64);
        // 32x32 RGBA needs 4096 bytes; four bytes must not reach gfx_image.
        c.image(0, 0, 32, 32, &[0u8; 4], 32, 32);
        c.image(0, 0, 32, 32, &[], 0, 32);
    }

    #[test]
    #[should_panic(expected = "pitch * height")]
    fn rejects_a_short_framebuffer() {
        let mut px = vec![0u32; 10];
        let _ = Canvas::new(&mut px, 10, 10, 10);
    }

    #[test]
    fn draws_facet_text() {
        let mut px = vec![0u32; 240 * 60];
        {
            let mut c = Canvas::new(&mut px, 240, 60, 240);
            c.clear(0x101020);
            c.round_rect(2, 2, 236, 56, 8, 0x202040);
        }
        let before = px.clone();
        {
            let mut c = Canvas::new(&mut px, 240, 60, 240);
            c.text(8, 8, "Settings", 0xFFFFFF, Font::Medium);
        }
        assert!(px.iter().zip(before.iter()).any(|(a, b)| a != b));
        assert!(text_width("Settings", Font::Medium) > 10);
    }
}
