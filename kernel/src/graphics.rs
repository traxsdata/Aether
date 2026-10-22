// SPDX-License-Identifier: Apache-2.0
//! Linear framebuffer graphics service.
//!
//! The loader hands over a linear, 32-bit GOP framebuffer. The kernel maps it
//! cache-disabled during paging setup and this module owns all pixel writes.
//! There is no GPU command stream: the first desktop paints a software
//! compositor into a bounded back buffer when the mode fits.

use crate::serial_line;
use aether_abi::{FramebufferInfo, PixelFormat};
use core::cell::UnsafeCell;

/// Exact truncating division by 255 for the 16-bit products used by the
/// eight-bit alpha blend.
///
/// The naive form costs three integer divisions per blended pixel; window
/// shadows alone blend millions of pixels per repaint. `n + 1 + (n >> 8)` is
/// exact for every `n` up to `255 * 255`.
#[inline]
const fn div255(value: u32) -> u32 {
    let value = value + 1;
    (value + (value >> 8)) >> 8
}

/// A source colour premultiplied for [`blend_premultiplied`].
///
/// Translucent fills blend one colour into many pixels, so the source half of
/// each channel product is computed once per primitive instead of per pixel.
#[derive(Clone, Copy)]
struct Premultiplied {
    /// Red and blue products in two 16-bit lanes.
    red_blue: u32,
    green: u32,
    inverse: u32,
}

impl Premultiplied {
    #[inline]
    const fn new(rgb: u32, alpha: u32) -> Self {
        Self {
            red_blue: (rgb & 0x00ff_00ff) * alpha,
            green: ((rgb >> 8) & 0xff) * alpha,
            inverse: 255 - alpha,
        }
    }
}

/// Blends a premultiplied source over `existing`, bit-identical to applying
/// [`div255`] to each channel separately.
///
/// Red and blue share one 32-bit word as two 16-bit lanes. Every lane sum is
/// at most `255 * 255`, and the `div255` steps stay below `0x1_0000`, so no
/// lane carries into its neighbour.
#[inline]
const fn blend_premultiplied(existing: u32, source: Premultiplied) -> u32 {
    let red_blue = (existing & 0x00ff_00ff) * source.inverse + source.red_blue + 0x0001_0001;
    let red_blue = ((red_blue + ((red_blue >> 8) & 0x00ff_00ff)) >> 8) & 0x00ff_00ff;
    let green = div255(((existing >> 8) & 0xff) * source.inverse + source.green);
    red_blue | (green << 8)
}

/// Blends `0x00RRGGBB` over `existing` with eight-bit alpha.
#[inline]
fn blend_rgb(existing: u32, rgb: u32, alpha: u32) -> u32 {
    blend_premultiplied(existing, Premultiplied::new(rgb, alpha))
}

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
    clip: Option<Clip>,
    round_clip: Option<(Clip, i32)>,
    damage: Option<Clip>,
    volatile: bool,
}

/// The real display surface used by [`present`].
#[derive(Clone, Copy)]
struct Front {
    base: *mut u8,
    width: u32,
    height: u32,
    stride: u32,
}

/// Maximum resolution held in the off-screen composition buffer.
///
/// A back buffer lets a frame complete before the display is touched, which
/// avoids exposing intermediate drawing operations. GOP has no vblank/page-flip
/// API, so the scanout can still tear during the final copy.
/// Larger modes fall back to direct painting rather than failing to boot.
const BACK_WIDTH: u32 = 1920;
const BACK_HEIGHT: u32 = 1200;
const BACK_PIXELS: usize = BACK_WIDTH as usize * BACK_HEIGHT as usize;

struct BackBuffer(UnsafeCell<[u32; BACK_PIXELS]>);

// SAFETY: Only the idle/boot context on the single CPU enters graphics; IRQs
// never render and no other scheduled task accesses these buffers.
unsafe impl Sync for BackBuffer {}

static BACK_BUFFER: BackBuffer = BackBuffer(UnsafeCell::new([0; BACK_PIXELS]));
static WALLPAPER: BackBuffer = BackBuffer(UnsafeCell::new([0; BACK_PIXELS]));
static WALLPAPER_VALID: Global<bool> = Global(UnsafeCell::new(false));

/// Pointer backing is tiny: moving it need not repaint or present a full screen.
pub const CURSOR_WIDTH: i32 = 15;
pub const CURSOR_HEIGHT: i32 = 22;
struct CursorBacking {
    rect: Option<Clip>,
    pixels: [u32; CURSOR_WIDTH as usize * CURSOR_HEIGHT as usize],
}
static CURSOR_BACKING: Global<CursorBacking> = Global(UnsafeCell::new(CursorBacking {
    rect: None,
    pixels: [0; CURSOR_WIDTH as usize * CURSOR_HEIGHT as usize],
}));

#[derive(Clone, Copy)]
struct Clip {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

impl Clip {
    fn contains(self, x: i32, y: i32) -> bool {
        x >= self.x
            && y >= self.y
            && i64::from(x) < i64::from(self.x) + i64::from(self.width)
            && i64::from(y) < i64::from(self.y) + i64::from(self.height)
    }
}

static FRAMEBUFFER: Global<Option<Framebuffer>> = Global(UnsafeCell::new(None));
static FRONT: Global<Option<Front>> = Global(UnsafeCell::new(None));

/// Graphics initialization failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphicsError {
    /// The framebuffer descriptor passed ABI validation but is unusable.
    UnsupportedFormat,
}

impl Framebuffer {
    #[inline]
    fn unpack(&self, rgb: u32) -> u32 {
        self.format.decode_rgb(rgb).unwrap_or(0)
    }

    #[inline]
    fn pack(&self, rgb: u32) -> u32 {
        self.format.encode_rgb(rgb).unwrap_or(0)
    }

    #[inline]
    fn visible(&self, x: i32, y: i32) -> bool {
        x >= 0
            && y >= 0
            && x < self.width as i32
            && y < self.height as i32
            && self.clip.is_none_or(|clip| clip.contains(x, y))
            && self.damage.is_none_or(|clip| clip.contains(x, y))
            && self.round_clip.is_none_or(|(clip, radius)| {
                if !clip.contains(x, y) {
                    return false;
                }
                let inset = rounded_inset(y - clip.y, clip.height, radius);
                x >= clip.x + inset && x < clip.x + clip.width - inset
            })
    }

    fn clipped(&self, x: i32, y: i32, width: i32, height: i32) -> Option<Clip> {
        if width <= 0 || height <= 0 {
            return None;
        }
        let mut left = i64::from(x).max(0);
        let mut top = i64::from(y).max(0);
        let mut right = (i64::from(x) + i64::from(width)).min(i64::from(self.width));
        let mut bottom = (i64::from(y) + i64::from(height)).min(i64::from(self.height));
        for clip in [
            self.clip,
            self.damage,
            self.round_clip.map(|(clip, _)| clip),
        ]
        .into_iter()
        .flatten()
        {
            left = left.max(i64::from(clip.x));
            top = top.max(i64::from(clip.y));
            right = right.min(i64::from(clip.x) + i64::from(clip.width));
            bottom = bottom.min(i64::from(clip.y) + i64::from(clip.height));
        }
        (left < right && top < bottom).then_some(Clip {
            x: left as i32,
            y: top as i32,
            width: (right - left) as i32,
            height: (bottom - top) as i32,
        })
    }

    fn row_span(&self, rect: Clip, row: i32) -> (i32, i32) {
        if let Some((clip, radius)) = self.round_clip {
            let inset = rounded_inset(row - clip.y, clip.height, radius);
            let left = rect.x.max(clip.x + inset);
            let right = (rect.x + rect.width).min(clip.x + clip.width - inset);
            (left, (right - left).max(0))
        } else {
            (rect.x, rect.width)
        }
    }

    #[inline]
    fn pixel(&mut self, x: i32, y: i32, rgb: u32) {
        if !self.visible(x, y) {
            return;
        }
        let offset = (y as usize * self.stride as usize + x as usize) * 4;
        // SAFETY: The ABI validated that the framebuffer covers
        // `stride * height * 4` bytes, and the bounds were checked above. The
        // back buffer has the same backing guarantee.
        unsafe {
            let pointer = self.base.add(offset) as *mut u32;
            if self.volatile {
                pointer.write_volatile(self.pack(rgb));
            } else {
                pointer.write(self.pack(rgb));
            }
        }
    }

    #[inline]
    fn pixel_blend(&mut self, x: i32, y: i32, rgb: u32, alpha: u8) {
        if !self.visible(x, y) || alpha == 0 {
            return;
        }
        if alpha == 255 {
            self.pixel(x, y, rgb);
            return;
        }
        let offset = (y as usize * self.stride as usize + x as usize) * 4;
        // SAFETY: Same invariant as `pixel`.
        unsafe {
            let pointer = self.base.add(offset) as *mut u32;
            let existing = if self.volatile {
                self.unpack(pointer.read_volatile())
            } else {
                self.unpack(pointer.read())
            };
            let blended = blend_rgb(existing, rgb, u32::from(alpha));
            if self.volatile {
                pointer.write_volatile(self.pack(blended));
            } else {
                pointer.write(self.pack(blended));
            }
        }
    }

    /// Copies one already-clipped destination row of a guest surface.
    ///
    /// Opaque rows are written as packed words without a per-pixel visibility
    /// or blend check. Translucent rows fall back to per-pixel blending so the
    /// window backdrop still shows through. `rect` and `row_span` have already
    /// bounded the destination to the current clip, damage region, and rounded
    /// window silhouette.
    fn blit_row(&mut self, x: i32, y: i32, source: &[u32], format: u32) {
        // The Linux content path holds interrupts while it copies a surface.
        // Read the PS/2 controller periodically so keystrokes are not lost.
        // Host render tests reuse this module without the kernel input service.
        #[cfg(target_os = "none")]
        if y & 31 == 0 {
            crate::input::service_controller();
        }
        let opaque = format == 0
            || source
                .iter()
                .all(|pixel| ((pixel >> 24) & 0xff) as u8 == 255);
        if !opaque || source.is_empty() {
            if !opaque {
                for (column, pixel) in source.iter().enumerate() {
                    let pixel = *pixel;
                    let alpha = ((pixel >> 24) & 0xff) as u8;
                    if alpha != 0 {
                        self.pixel_blend(x + column as i32, y, pixel & 0x00ff_ffff, alpha);
                    }
                }
            }
            return;
        }
        let offset = y as usize * self.stride as usize + x as usize;
        // SAFETY: The caller clipped the row to this framebuffer's validated
        // bounds. Source and destination are distinct kernel-owned buffers.
        unsafe {
            let pointer = (self.base as *mut u32).add(offset);
            if self.volatile {
                for (column, pixel) in source.iter().enumerate() {
                    pointer
                        .add(column)
                        .write_volatile(self.pack(pixel & 0x00ff_ffff));
                }
            } else if self.format == PixelFormat::BlueGreenRedReserved {
                // The common GOP layout needs no channel permutation; only the
                // reserved X byte is cleared.
                for (column, pixel) in source.iter().enumerate() {
                    pointer.add(column).write(pixel & 0x00ff_ffff);
                }
            } else {
                for (column, pixel) in source.iter().enumerate() {
                    pointer.add(column).write(self.pack(pixel & 0x00ff_ffff));
                }
            }
        }
    }

    fn rect(&mut self, x: i32, y: i32, width: i32, height: i32, rgb: u32) {
        let Some(rect) = self.clipped(x, y, width, height) else {
            return;
        };
        let packed = self.pack(rgb);
        for row in rect.y..rect.y + rect.height {
            let (left, width) = self.row_span(rect, row);
            if width == 0 {
                continue;
            }
            // SAFETY: `clipped` bounds each row to the validated surface.
            unsafe {
                let pointer = (self.base as *mut u32)
                    .add(row as usize * self.stride as usize + left as usize);
                if self.volatile {
                    for column in 0..width as usize {
                        pointer.add(column).write_volatile(packed);
                    }
                } else {
                    core::slice::from_raw_parts_mut(pointer, width as usize).fill(packed);
                }
            }
        }
    }

    fn rect_blend(&mut self, x: i32, y: i32, width: i32, height: i32, rgb: u32, alpha: u8) {
        if alpha == 0 {
            return;
        }
        if alpha == 255 {
            self.rect(x, y, width, height, rgb);
            return;
        }
        let Some(rect) = self.clipped(x, y, width, height) else {
            return;
        };
        // Blend native words directly: channel permutation commutes with alpha
        // blending, so RGB/BGR conversion is needed only once per rectangle.
        let source = Premultiplied::new(self.pack(rgb), u32::from(alpha));
        for row in rect.y..rect.y + rect.height {
            let (left, width) = self.row_span(rect, row);
            if width == 0 {
                continue;
            }
            // SAFETY: `clipped` and `row_span` bound the row to the surface.
            unsafe {
                let pointer = (self.base as *mut u32)
                    .add(row as usize * self.stride as usize + left as usize);
                if self.volatile {
                    for column in 0..width as usize {
                        let target = pointer.add(column);
                        target.write_volatile(blend_premultiplied(target.read_volatile(), source));
                    }
                } else {
                    for pixel in core::slice::from_raw_parts_mut(pointer, width as usize) {
                        *pixel = blend_premultiplied(*pixel, source);
                    }
                }
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
    if !format.is_linear() || info.validate().is_err() {
        return Err(GraphicsError::UnsupportedFormat);
    }
    // SAFETY: The framebuffer is mapped cache-disabled by the paging layer and
    // the ABI validated its length. The back buffer is static and exclusively
    // owned by this module.
    unsafe {
        *WALLPAPER_VALID.0.get() = false;
        (*CURSOR_BACKING.0.get()).rect = None;
        *FRONT.0.get() = Some(Front {
            base: info.base as *mut u8,
            width: info.width,
            height: info.height,
            stride: info.stride,
        });
        let use_back_buffer = info.width <= BACK_WIDTH && info.height <= BACK_HEIGHT;
        let (base, stride, volatile) = if use_back_buffer {
            (BACK_BUFFER.0.get() as *mut u8, info.width, false)
        } else {
            (info.base as *mut u8, info.stride, true)
        };
        *FRAMEBUFFER.0.get() = Some(Framebuffer {
            base,
            width: info.width,
            height: info.height,
            stride,
            format,
            clip: None,
            round_clip: None,
            damage: None,
            volatile,
        });
    }
    serial_line!(
        "graphics: framebuffer {}x{} format={:?} backbuffer={}",
        info.width,
        info.height,
        format,
        if info.width <= BACK_WIDTH && info.height <= BACK_HEIGHT {
            "on"
        } else {
            "direct"
        }
    );
    Ok(true)
}

/// Returns `true` if a linear framebuffer is available.
#[must_use]
pub fn is_ready() -> bool {
    // SAFETY: The slot is written once during boot and then read-only.
    unsafe { (*FRONT.0.get()).is_some() }
}

/// Returns the visible width.
#[must_use]
pub fn width() -> u32 {
    // SAFETY: Read-only access to the boot-time descriptor.
    unsafe { (*FRONT.0.get()).as_ref().map_or(0, |front| front.width) }
}

/// Returns the visible height.
#[must_use]
pub fn height() -> u32 {
    // SAFETY: Read-only access to the boot-time descriptor.
    unsafe { (*FRONT.0.get()).as_ref().map_or(0, |front| front.height) }
}

fn with_framebuffer<R>(f: impl FnOnce(&mut Framebuffer) -> R) -> Option<R> {
    // SAFETY: All callers run on the single CPU and do not hold another
    // mutable reference across this call.
    unsafe { (*FRAMEBUFFER.0.get()).as_mut().map(f) }
}

/// Fills a rectangle, clipped to the screen.
pub fn fill_rect(x: i32, y: i32, width: i32, height: i32, rgb: u32) {
    let _ = with_framebuffer(|fb| fb.rect(x, y, width, height, rgb));
}

/// Blends a translucent rectangle over the framebuffer.
pub fn blend_rect(x: i32, y: i32, width: i32, height: i32, rgb: u32, alpha: u8) {
    let _ = with_framebuffer(|fb| fb.rect_blend(x, y, width, height, rgb, alpha));
}

/// Restricts all subsequent drawing to a rectangle.
pub fn set_clip(x: i32, y: i32, width: i32, height: i32) {
    let _ = with_framebuffer(|fb| {
        fb.clip = Some(Clip {
            x,
            y,
            width: width.max(0),
            height: height.max(0),
        });
    });
}

/// Removes the current clip rectangle.
pub fn clear_clip() {
    let _ = with_framebuffer(|fb| fb.clip = None);
}

/// Clips content to the outer rounded window silhouette, independently of its
/// rectangular content clip and the frame's damage region.
pub fn set_round_clip(x: i32, y: i32, width: i32, height: i32, radius: i32) {
    let radius = radius.max(0).min(width.max(0) / 2).min(height.max(0) / 2);
    let _ = with_framebuffer(|fb| {
        fb.round_clip = Some((
            Clip {
                x,
                y,
                width,
                height,
            },
            radius,
        ))
    });
}

/// Ends rounded window content clipping.
pub fn clear_round_clip() {
    let _ = with_framebuffer(|fb| fb.round_clip = None);
}

/// Sets the persistent frame damage clip. Inner window clips cannot widen it.
pub fn set_damage(x: i32, y: i32, width: i32, height: i32) {
    let _ = with_framebuffer(|fb| {
        fb.damage = Some(Clip {
            x,
            y,
            width: width.max(0),
            height: height.max(0),
        })
    });
}

/// Ends damage-clipped painting before cursor composition/presentation.
pub fn clear_damage() {
    let _ = with_framebuffer(|fb| fb.damage = None);
}

/// Caches the wallpaper in normal RAM. Only call after painting the full screen.
pub fn cache_wallpaper() {
    let _ = with_framebuffer(|fb| {
        if fb.volatile {
            return;
        }
        // SAFETY: Back-buffer modes fit both disjoint static arrays.
        unsafe {
            core::ptr::copy_nonoverlapping(
                fb.base as *const u32,
                WALLPAPER.0.get() as *mut u32,
                fb.width as usize * fb.height as usize,
            );
            *WALLPAPER_VALID.0.get() = true;
        }
    });
}

/// Restores just the damaged wallpaper region. Returns false in direct mode.
pub fn restore_wallpaper(x: i32, y: i32, width: i32, height: i32) -> bool {
    with_framebuffer(|fb| {
        // SAFETY: The flag is private to the single-owner graphics service.
        if fb.volatile || !unsafe { *WALLPAPER_VALID.0.get() } {
            return false;
        }
        if let Some(rect) = fb.clipped(x, y, width, height) {
            for row in rect.y..rect.y + rect.height {
                let offset = row as usize * fb.stride as usize + rect.x as usize;
                // SAFETY: Both RAM buffers share the validated packed stride.
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        (WALLPAPER.0.get() as *const u32).add(offset),
                        (fb.base as *mut u32).add(offset),
                        rect.width as usize,
                    );
                }
            }
        }
        true
    })
    .unwrap_or(false)
}

/// Saves the scene under the cursor before drawing it into the back buffer.
pub fn save_cursor_background(x: i32, y: i32) {
    let _ = with_framebuffer(|fb| {
        if fb.volatile {
            return;
        }
        let rect = fb.clipped(x, y, CURSOR_WIDTH, CURSOR_HEIGHT);
        // SAFETY: The graphics service is the only owner, and clipping limits
        // the copy to the cursor array and the validated RAM back buffer.
        unsafe {
            let backing = &mut *CURSOR_BACKING.0.get();
            backing.rect = rect;
            if let Some(rect) = rect {
                for row in 0..rect.height as usize {
                    core::ptr::copy_nonoverlapping(
                        (fb.base as *const u32)
                            .add((rect.y as usize + row) * fb.stride as usize + rect.x as usize),
                        backing.pixels.as_mut_ptr().add(row * CURSOR_WIDTH as usize),
                        rect.width as usize,
                    );
                }
            }
        }
    });
}

/// Erases the previous cursor and returns its old presentation bounds.
pub fn restore_cursor_background() -> Option<(i32, i32, i32, i32)> {
    with_framebuffer(|fb| {
        if fb.volatile {
            return None;
        }
        // SAFETY: Bounds were clipped by `save_cursor_background`; the backing
        // store is exclusively owned by this service on the single CPU.
        unsafe {
            let backing = &mut *CURSOR_BACKING.0.get();
            let rect = backing.rect.take()?;
            for row in 0..rect.height as usize {
                core::ptr::copy_nonoverlapping(
                    backing.pixels.as_ptr().add(row * CURSOR_WIDTH as usize),
                    (fb.base as *mut u32)
                        .add((rect.y as usize + row) * fb.stride as usize + rect.x as usize),
                    rect.width as usize,
                );
            }
            Some((rect.x, rect.y, rect.width, rect.height))
        }
    })
    .flatten()
}

/// Fills a rectangle with a vertical colour gradient.
pub fn vertical_gradient(x: i32, y: i32, width: i32, height: i32, top: u32, bottom: u32) {
    if width <= 0 || height <= 0 {
        return;
    }
    let _ = with_framebuffer(|fb| {
        for row in 0..height {
            let amount = if height <= 1 {
                0
            } else {
                (i64::from(row) * 256 / i64::from(height - 1)) as u32
            };
            let inverse = 256 - amount;
            let channel = |shift: u32| -> u32 {
                (((top >> shift) & 0xff) * inverse + ((bottom >> shift) & 0xff) * amount) / 256
            };
            let rgb = (channel(16) << 16) | (channel(8) << 8) | channel(0);
            fb.rect(x, y + row, width, 1, rgb);
        }
    });
}

/// Fills a circle with a hard edge.
pub fn fill_circle(center_x: i32, center_y: i32, radius: i32, rgb: u32) {
    if radius <= 0 {
        return;
    }
    let _ = with_framebuffer(|fb| {
        for row in -radius..=radius {
            let span = square_root(radius * radius - row * row);
            fb.rect(center_x - span, center_y + row, span * 2 + 1, 1, rgb);
        }
    });
}

/// Fills a translucent circle.
pub fn blend_circle(center_x: i32, center_y: i32, radius: i32, rgb: u32, alpha: u8) {
    if radius <= 0 {
        return;
    }
    let _ = with_framebuffer(|fb| {
        for row in -radius..=radius {
            let span = square_root(radius * radius - row * row);
            fb.rect_blend(center_x - span, center_y + row, span * 2 + 1, 1, rgb, alpha);
        }
    });
}

/// Fills a rounded rectangle.
pub fn fill_round_rect(x: i32, y: i32, width: i32, height: i32, radius: i32, rgb: u32) {
    rounded_spans(
        x,
        y,
        width,
        height,
        radius,
        |left, top, span, row_height| {
            fill_rect(left, top, span, row_height, rgb);
        },
    );
}

/// Fills a translucent rounded rectangle.
pub fn blend_round_rect(x: i32, y: i32, width: i32, height: i32, radius: i32, rgb: u32, alpha: u8) {
    rounded_spans(
        x,
        y,
        width,
        height,
        radius,
        |left, top, span, row_height| {
            blend_rect(left, top, span, row_height, rgb, alpha);
        },
    );
}

/// Fills a translucent rounded rectangle except inside `hole`.
///
/// Window shadows and translucent bodies lie mostly under opaque content that
/// is painted over them in the same frame. Blending those pixels first is
/// invisible work, and on a large window it dominates the frame. Pixels
/// outside the hole are blended exactly as [`blend_round_rect`] would.
#[allow(clippy::too_many_arguments)]
pub fn blend_round_rect_around(
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    radius: i32,
    rgb: u32,
    alpha: u8,
    hole: (i32, i32, i32, i32),
) {
    let (hole_x, hole_y, hole_width, hole_height) = hole;
    if hole_width <= 0 || hole_height <= 0 {
        blend_round_rect(x, y, width, height, radius, rgb, alpha);
        return;
    }
    let hole_right = hole_x.saturating_add(hole_width);
    let hole_bottom = hole_y.saturating_add(hole_height);
    rounded_spans(
        x,
        y,
        width,
        height,
        radius,
        |left, top, span, row_height| {
            if top < hole_y || top >= hole_bottom {
                blend_rect(left, top, span, row_height, rgb, alpha);
                return;
            }
            let right = left + span;
            if left < hole_x {
                blend_rect(
                    left,
                    top,
                    (right.min(hole_x) - left).max(0),
                    row_height,
                    rgb,
                    alpha,
                );
            }
            if right > hole_right {
                let start = left.max(hole_right);
                blend_rect(start, top, right - start, row_height, rgb, alpha);
            }
        },
    );
}

/// Draws a one-pixel rounded outline.
pub fn outline_round_rect(x: i32, y: i32, width: i32, height: i32, radius: i32, rgb: u32) {
    if width <= 0 || height <= 0 {
        return;
    }
    let radius = radius.min(width / 2).min(height / 2).max(0);
    let _ = with_framebuffer(|fb| {
        for row in 0..height {
            let inset = rounded_inset(row, height, radius);
            fb.pixel(x + inset, y + row, rgb);
            fb.pixel(x + width - 1 - inset, y + row, rgb);
            if row < 2 || row >= height - 2 {
                fb.rect(x + inset, y + row, width - inset * 2, 1, rgb);
            }
        }
    });
}

/// Fills a convex or concave polygon using a scanline fill.
pub fn fill_polygon(points: &[(i32, i32)], rgb: u32) {
    if points.len() < 3 {
        return;
    }
    let mut top = points[0].1;
    let mut bottom = points[0].1;
    for point in points {
        top = top.min(point.1);
        bottom = bottom.max(point.1);
    }
    if bottom < top {
        return;
    }
    let mut intersections = [0i32; 24];
    let _ = with_framebuffer(|fb| {
        for scanline in top..=bottom {
            let mut count = 0usize;
            for index in 0..points.len() {
                let first = points[index];
                let second = points[(index + 1) % points.len()];
                let (low, high) = if first.1 < second.1 {
                    (first, second)
                } else {
                    (second, first)
                };
                if scanline < low.1 || scanline >= high.1 || high.1 == low.1 {
                    continue;
                }
                let x = i64::from(low.0)
                    + (i64::from(scanline - low.1) * i64::from(high.0 - low.0))
                        / i64::from(high.1 - low.1);
                if count < intersections.len() {
                    intersections[count] = x as i32;
                    count += 1;
                }
            }
            for left in 0..count {
                for right in (left + 1)..count {
                    if intersections[right] < intersections[left] {
                        intersections.swap(left, right);
                    }
                }
            }
            let mut pair = 0;
            while pair + 1 < count {
                let start = intersections[pair];
                let end = intersections[pair + 1];
                fb.rect(start, scanline, (end - start).max(1), 1, rgb);
                pair += 2;
            }
        }
    });
}

/// Returns the horizontal width of `text` at a scale.
#[must_use]
pub fn text_width(text: &str, scale: i32) -> i32 {
    text.chars().count() as i32 * 6 * scale.max(1)
}

fn square_root(value: i32) -> i32 {
    if value <= 0 {
        return 0;
    }
    let mut current = value;
    let mut next = (current + 1) / 2;
    while next < current {
        current = next;
        next = (current + value / current) / 2;
    }
    current
}

fn rounded_inset(row: i32, height: i32, radius: i32) -> i32 {
    if radius <= 0 || height <= 0 {
        return 0;
    }
    let dy = if row < radius {
        radius - row
    } else if row >= height - radius {
        row - (height - radius) + 1
    } else {
        return 0;
    };
    radius - square_root(radius * radius - dy * dy)
}

fn rounded_spans(
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    radius: i32,
    mut paint: impl FnMut(i32, i32, i32, i32),
) {
    if width <= 0 || height <= 0 {
        return;
    }
    let radius = radius.min(width / 2).min(height / 2).max(0);
    for row in 0..height {
        let inset = rounded_inset(row, height, radius);
        paint(x + inset, y + row, (width - inset * 2).max(1), 1);
    }
}

/// Draws a one-pixel line.
pub fn draw_line(x0: i32, y0: i32, x1: i32, y1: i32, rgb: u32) {
    let dx = (x1 - x0).abs();
    let step_x = if x0 < x1 { 1 } else { -1 };
    let dy = -(y1 - y0).abs();
    let step_y = if y0 < y1 { 1 } else { -1 };
    let mut error = dx + dy;
    let mut x = x0;
    let mut y = y0;
    let _ = with_framebuffer(|fb| {
        loop {
            fb.pixel(x, y, rgb);
            if x == x1 && y == y1 {
                break;
            }
            let doubled = error * 2;
            if doubled >= dy {
                error += dy;
                x += step_x;
            }
            if doubled <= dx {
                error += dx;
                y += step_y;
            }
        }
    });
}

/// Draws a one-pixel circle outline.
pub fn stroke_circle(center_x: i32, center_y: i32, radius: i32, rgb: u32) {
    if radius <= 0 {
        return;
    }
    let _ = with_framebuffer(|fb| {
        let mut x = radius;
        let mut y = 0;
        let mut error = 1 - radius;
        while x >= y {
            for (px, py) in [
                (x, y),
                (y, x),
                (-y, x),
                (-x, y),
                (-x, -y),
                (-y, -x),
                (y, -x),
                (x, -y),
            ] {
                fb.pixel(center_x + px, center_y + py, rgb);
            }
            y += 1;
            if error < 0 {
                error += 2 * y + 1;
            } else {
                x -= 1;
                error += 2 * (y - x) + 1;
            }
        }
    });
}

/// Fills a radial glow with a quadratic falloff.
pub fn blend_glow(center_x: i32, center_y: i32, radius: i32, rgb: u32, alpha: u8) {
    if radius <= 0 || alpha == 0 {
        return;
    }
    let squared = i64::from(radius) * i64::from(radius);
    let _ = with_framebuffer(|fb| {
        for row in -radius..=radius {
            for column in -radius..=radius {
                let distance =
                    i64::from(column) * i64::from(column) + i64::from(row) * i64::from(row);
                if distance >= squared {
                    continue;
                }
                let falloff = ((squared - distance) * 256 / squared) as u32;
                let strength = u32::from(alpha) * falloff / 256;
                if strength > 0 {
                    fb.pixel_blend(center_x + column, center_y + row, rgb, strength as u8);
                }
            }
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
        '&' => [0x0c, 0x12, 0x14, 0x08, 0x15, 0x12, 0x0d],
        '·' => [0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00],
        '#' => [0x0a, 0x0a, 0x1f, 0x0a, 0x1f, 0x0a, 0x0a],
        '\'' => [0x04, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00],
        '"' => [0x0a, 0x0a, 0x00, 0x00, 0x00, 0x00, 0x00],
        '@' => [0x0e, 0x11, 0x17, 0x15, 0x17, 0x10, 0x0e],
        _ => [0x00; 7],
    }
}

/// Blits a tightly packed guest surface into a destination rectangle.
///
/// Sampling is nearest-neighbour and clipped by the framebuffer. `src` is
/// kernel-owned copied pixels, never a guest pointer. `format` is 0 for
/// XRGB8888 and 1 for ARGB8888, matching the display endpoint ABI.
#[allow(clippy::too_many_arguments)]
pub fn blit_surface_scaled(
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    source: &[u32],
    source_width: u32,
    source_height: u32,
    format: u32,
) {
    if width <= 0 || height <= 0 || source_width == 0 || source_height == 0 {
        return;
    }
    if source.len() < source_width as usize * source_height as usize {
        return;
    }
    let _ = with_framebuffer(|fb| {
        // Clip once instead of asking every source pixel to reject itself.
        // Without this, an unrelated menu-bar or clock repaint still walks the
        // full Linux surface and takes the interrupt guard for nothing.
        let Some(rect) = fb.clipped(x, y, width, height) else {
            return;
        };
        // The common case is an unscaled surface. Avoid two integer divisions
        // per pixel and walk contiguous source rows instead.
        if width as u32 == source_width && height as u32 == source_height {
            let stride = source_width as usize;
            for row in rect.y..rect.y + rect.height {
                let (left, span) = fb.row_span(rect, row);
                if span == 0 {
                    continue;
                }
                let start = (row - y) as usize * stride + (left - x) as usize;
                fb.blit_row(left, row, &source[start..start + span as usize], format);
            }
            return;
        }
        // Scaled presentation is uncommon (the window is clamped or resized),
        // but clip the loop and keep the source mapping proportional.
        for row in rect.y..rect.y + rect.height {
            let source_y = ((row - y) as u32 * source_height / height as u32) as usize;
            let (left, span) = fb.row_span(rect, row);
            for column in left..left + span {
                let source_x = ((column - x) as u32 * source_width / width as u32) as usize;
                let pixel = source[source_y * source_width as usize + source_x];
                let rgb = pixel & 0x00ff_ffff;
                if format == 1 {
                    let alpha = ((pixel >> 24) & 0xff) as u8;
                    if alpha == 0 {
                        continue;
                    }
                    if alpha < 255 {
                        fb.pixel_blend(column, row, rgb, alpha);
                    } else {
                        fb.pixel(column, row, rgb);
                    }
                } else {
                    fb.pixel(column, row, rgb);
                }
            }
        }
    });
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

/// Copies the completed back buffer to the display.
///
/// This is the only path that writes the real framebuffer when a back buffer is
/// active. This avoids copying intermediate drawing operations, but is not an
/// atomic page flip and cannot guarantee tear-free scanout without vblank.
pub fn present() {
    present_region(0, 0, width() as i32, height() as i32);
}

/// Presents only damaged pixels, preserving firmware scan-line padding.
pub fn present_region(x: i32, y: i32, width: i32, height: i32) {
    // SAFETY: Both descriptors are boot-time constants and the copy stays
    // within each surface's validated dimensions.
    unsafe {
        let Some(draw) = (*FRAMEBUFFER.0.get()).as_ref() else {
            return;
        };
        let Some(front) = (*FRONT.0.get()).as_ref() else {
            return;
        };
        if draw.base == front.base {
            return;
        }
        let source = draw.base as *const u32;
        let target = front.base as *mut u32;
        let Some(rect) = draw.clipped(x, y, width, height) else {
            return;
        };
        // Copy each damaged row as one block. A pixel-at-a-time volatile loop
        // is the dominant cost when the software-rendered Linux surface
        // repaints, especially on emulated/virtual framebuffers.
        for row in rect.y as usize..(rect.y + rect.height) as usize {
            let source_row = source.add(row * draw.stride as usize + rect.x as usize);
            let target_row = target.add(row * front.stride as usize + rect.x as usize);
            core::ptr::copy_nonoverlapping(source_row, target_row, rect.width as usize);
        }
    }
}

/// Returns a deterministic checksum of sampled framebuffer pixels.
///
/// This is the evidence the boot test uses to prove the compositor actually
/// wrote pixels, without requiring a screenshot. It samples the presented
/// surface, not the back buffer.
#[must_use]
pub fn checksum() -> u32 {
    // SAFETY: Read-only access to a validated surface.
    let Some(front) = (unsafe { (*FRONT.0.get()).as_ref() }) else {
        return 0;
    };
    let mut hash = 0x811c_9dc5u32;
    let step_x = (front.width / 16).max(1);
    let step_y = (front.height / 12).max(1);
    let mut y = 0;
    while y < front.height {
        let mut x = 0;
        while x < front.width {
            let offset = (y as usize * front.stride as usize + x as usize) * 4;
            // SAFETY: Bounds are inside the validated framebuffer.
            let value = unsafe { (front.base.add(offset) as *const u32).read_volatile() };
            hash ^= value;
            hash = hash.wrapping_mul(0x0100_0193);
            x += step_x;
        }
        y += step_y;
    }
    hash
}
