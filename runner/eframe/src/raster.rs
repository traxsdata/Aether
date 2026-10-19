//! raster.rs - the software rasterizer: draws egui's tessellated meshes into a
//! 0x00RRGGBB pixel buffer.
//!
//! It does what egui-wgpu's shader does, in integers: colours are sRGB-gamma
//! and premultiplied (egui's `Color32`), a fragment is vertex colour times
//! texel, and blending is `dst = src + dst * (1 - src.a)` in gamma space.
//!
//! Triangles are scanned row by row inside their clipped bounding box: each
//! row's span comes straight from the three integer edge functions (8-bit
//! sub-pixel precision, top-left fill rule), so no pixel is tested twice.
//! Pairs of triangles that form an axis-aligned rectangle with flat colour
//! (panels, frames, text glyphs, images) skip edge functions entirely.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

use egui::epaint::{ClippedPrimitive, ImageDelta, ImageData, Mesh, Primitive, Vertex};
use egui::{TextureFilter, TextureId, TextureOptions, TextureWrapMode};

/// A texture: premultiplied RGBA texels, `r | g << 8 | b << 16 | a << 24`.
pub struct Texture {
    pub w: u32,
    pub h: u32,
    pub px: Vec<u32>,
    pub options: TextureOptions,
}

/// Rows `y0..y0 + h` of the window: what one call to `paint` may touch.
pub struct Target<'a> {
    pub px: &'a mut [u32],
    pub width: usize,
    pub height: usize,
    /// The first row of `px` in window coordinates.
    pub y0: i32,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    pub meshes: u32,
    pub triangles: u32,
    pub rects: u32,
    pub callbacks: u32,
}

#[derive(Default)]
pub struct Rasterizer {
    textures: HashMap<TextureId, Texture>,
    warned_callback: AtomicBool,
    warned_texture: AtomicBool,
}

/// An integer clip rectangle in pixels, max exclusive.
#[derive(Clone, Copy, Debug)]
struct Clip {
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
}

#[inline(always)]
fn div255(x: u32) -> u32 {
    // Exact round(x / 255) for x <= 255 * 255.
    let x = x + 128;
    (x + (x >> 8)) >> 8
}

/// Premultiplied src (each channel 0..=255) over an opaque 0x00RRGGBB pixel.
#[inline(always)]
fn blend(d: u32, r: u32, g: u32, b: u32, a: u32) -> u32 {
    if a >= 255 && r <= 255 && g <= 255 && b <= 255 {
        return (r << 16) | (g << 8) | b;
    }
    let inv = 255 - a.min(255);
    let dr = (d >> 16) & 0xFF;
    let dg = (d >> 8) & 0xFF;
    let db = d & 0xFF;
    let nr = (r + div255(dr * inv)).min(255);
    let ng = (g + div255(dg * inv)).min(255);
    let nb = (b + div255(db * inv)).min(255);
    (nr << 16) | (ng << 8) | nb
}

/// Vertex colour times texel, both premultiplied: returns (r, g, b, a).
#[inline(always)]
fn modulate(c: [u32; 4], t: u32) -> (u32, u32, u32, u32) {
    (
        div255(c[0] * (t & 0xFF)),
        div255(c[1] * ((t >> 8) & 0xFF)),
        div255(c[2] * ((t >> 16) & 0xFF)),
        div255(c[3] * (t >> 24)),
    )
}

/// Linear interpolation of two packed RGBA texels, `t` in 0..=256.
#[inline(always)]
fn lerp_px(a: u32, b: u32, t: u32) -> u32 {
    let s = 256 - t;
    let rb = (((a & 0x00FF_00FF) * s + (b & 0x00FF_00FF) * t) >> 8) & 0x00FF_00FF;
    let ag = ((((a >> 8) & 0x00FF_00FF) * s + ((b >> 8) & 0x00FF_00FF) * t) >> 8) & 0x00FF_00FF;
    rb | (ag << 8)
}

#[inline(always)]
fn wrap(i: i64, n: i64, mode: TextureWrapMode) -> usize {
    (match mode {
        TextureWrapMode::ClampToEdge => i.clamp(0, n - 1),
        TextureWrapMode::Repeat => i.rem_euclid(n),
        TextureWrapMode::MirroredRepeat => {
            let m = i.rem_euclid(2 * n);
            if m < n {
                m
            } else {
                2 * n - 1 - m
            }
        }
    }) as usize
}

impl Texture {
    /// The filter that applies when one pixel steps `scale` texels.
    fn filter(&self, scale: f32) -> TextureFilter {
        if scale > 1.0 {
            self.options.minification
        } else {
            self.options.magnification
        }
    }

    /// Samples at texel coordinates in 16.16 fixed point (texel centres at +0.5).
    #[inline(always)]
    fn sample(&self, fx: i64, fy: i64, filter: TextureFilter) -> u32 {
        let (w, h) = (self.w as i64, self.h as i64);
        let mode = self.options.wrap_mode;
        match filter {
            TextureFilter::Nearest => {
                let x = wrap(fx >> 16, w, mode);
                let y = wrap(fy >> 16, h, mode);
                self.px[y * self.w as usize + x]
            }
            TextureFilter::Linear => {
                let (gx, gy) = (fx - 0x8000, fy - 0x8000);
                let (ix, iy) = (gx >> 16, gy >> 16);
                let tx = ((gx >> 8) & 0xFF) as u32;
                let ty = ((gy >> 8) & 0xFF) as u32;
                let x0 = wrap(ix, w, mode);
                let x1 = wrap(ix + 1, w, mode);
                let r0 = wrap(iy, h, mode) * self.w as usize;
                let r1 = wrap(iy + 1, h, mode) * self.w as usize;
                let top = lerp_px(self.px[r0 + x0], self.px[r0 + x1], tx);
                let bot = lerp_px(self.px[r1 + x0], self.px[r1 + x1], tx);
                lerp_px(top, bot, ty)
            }
        }
    }
}

fn color_of(c: egui::Color32) -> [u32; 4] {
    [c.r() as u32, c.g() as u32, c.b() as u32, c.a() as u32]
}

/// Two triangles that tile an axis-aligned rectangle with one flat colour
/// and a UV mapping that follows the axes.
struct RectQuad {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    u0: f32,
    v0: f32,
    u1: f32,
    v1: f32,
    color: egui::Color32,
}

fn as_rect(v: &[Vertex], ix: &[u32]) -> Option<RectQuad> {
    let a = [ix[0], ix[1], ix[2]];
    let b = [ix[3], ix[4], ix[5]];
    let in_b = |i: u32| b.contains(&i);
    let shared: Vec<u32> = a.iter().copied().filter(|&i| in_b(i)).collect();
    if shared.len() != 2 || shared[0] == shared[1] {
        return None;
    }
    let pa = *a.iter().find(|&&i| !in_b(i))?;
    let pb = *b.iter().find(|&&i| !a.contains(&i))?;
    let (s1, s2) = (v.get(shared[0] as usize)?, v.get(shared[1] as usize)?);
    let (q1, q2) = (v.get(pa as usize)?, v.get(pb as usize)?);
    // The shared edge must be the diagonal, so the halves don't overlap.
    if s1.pos.x == s2.pos.x || s1.pos.y == s2.pos.y {
        return None;
    }
    let c1 = egui::pos2(s1.pos.x, s2.pos.y);
    let c2 = egui::pos2(s2.pos.x, s1.pos.y);
    if !((q1.pos == c1 && q2.pos == c2) || (q1.pos == c2 && q2.pos == c1)) {
        return None;
    }
    let color = s1.color;
    if s2.color != color || q1.color != color || q2.color != color {
        return None;
    }
    let (x0, x1) = (s1.pos.x.min(s2.pos.x), s1.pos.x.max(s2.pos.x));
    let (y0, y1) = (s1.pos.y.min(s2.pos.y), s1.pos.y.max(s2.pos.y));
    let (lo, hi) = if s1.pos.x == x0 { (s1, s2) } else { (s2, s1) };
    let (u0, u1) = (lo.uv.x, hi.uv.x);
    let (top, bot) = if s1.pos.y == y0 { (s1, s2) } else { (s2, s1) };
    let (v0, v1) = (top.uv.y, bot.uv.y);
    for q in [s1, s2, q1, q2] {
        let u = if q.pos.x == x0 { u0 } else { u1 };
        let vv = if q.pos.y == y0 { v0 } else { v1 };
        if q.uv.x != u || q.uv.y != vv {
            return None;
        }
    }
    Some(RectQuad { x0, y0, x1, y1, u0, v0, u1, v1, color })
}

impl Rasterizer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn texture(&self, id: TextureId) -> Option<&Texture> {
        self.textures.get(&id)
    }

    /// Applies one `ImageDelta`: a whole new image, or a patch at `pos`.
    pub fn set_texture(&mut self, id: TextureId, delta: &ImageDelta) {
        let ImageData::Color(img) = &delta.image;
        let [w, h] = img.size;
        let texels = img.pixels.iter().map(|c| u32::from_le_bytes(c.to_array()));
        match delta.pos {
            None => {
                self.textures.insert(
                    id,
                    Texture { w: w as u32, h: h as u32, px: texels.collect(), options: delta.options },
                );
            }
            Some([px, py]) => {
                let Some(t) = self.textures.get_mut(&id) else {
                    log::warn!("partial update of unknown texture {id:?}");
                    return;
                };
                t.options = delta.options;
                let tw = t.w as usize;
                for (i, c) in texels.enumerate() {
                    let (x, y) = (px + i % w.max(1), py + i / w.max(1));
                    if x < tw && y < t.h as usize {
                        t.px[y * tw + x] = c;
                    }
                }
            }
        }
    }

    pub fn free_texture(&mut self, id: TextureId) {
        self.textures.remove(&id);
    }

    /// Draws `prims` (in points) at `ppp` pixels per point into `t`.
    /// Bands of one frame can be painted from several threads at once.
    pub fn paint(&self, t: &mut Target<'_>, prims: &[ClippedPrimitive], ppp: f32) -> Stats {
        let mut stats = Stats::default();
        let band = Clip { x0: 0, y0: t.y0, x1: t.width as i32, y1: t.y0 + t.height as i32 };
        for p in prims {
            // As egui-wgpu's scissor: the clip rectangle rounded to pixels.
            let r = p.clip_rect;
            let clip = Clip {
                x0: ((r.min.x * ppp).round() as i32).max(band.x0),
                y0: ((r.min.y * ppp).round() as i32).max(band.y0),
                x1: ((r.max.x * ppp).round() as i32).min(band.x1),
                y1: ((r.max.y * ppp).round() as i32).min(band.y1),
            };
            if clip.x0 >= clip.x1 || clip.y0 >= clip.y1 {
                continue;
            }
            match &p.primitive {
                Primitive::Mesh(mesh) => self.paint_mesh(t, clip, mesh, ppp, &mut stats),
                Primitive::Callback(_) => {
                    stats.callbacks += 1;
                    if !self.warned_callback.swap(true, Relaxed) {
                        log::warn!("paint callbacks (custom GPU drawing) are not supported by the software renderer; skipping them");
                    }
                }
            }
        }
        stats
    }

    fn paint_mesh(&self, t: &mut Target<'_>, clip: Clip, mesh: &Mesh, ppp: f32, stats: &mut Stats) {
        let Some(tex) = self.textures.get(&mesh.texture_id) else {
            if !self.warned_texture.swap(true, Relaxed) {
                log::warn!("mesh uses unknown texture {:?}; skipping", mesh.texture_id);
            }
            return;
        };
        stats.meshes += 1;
        let (idx, v) = (&mesh.indices, &mesh.vertices);
        let mut i = 0;
        while i + 3 <= idx.len() {
            if i + 6 <= idx.len() {
                if let Some(q) = as_rect(v, &idx[i..i + 6]) {
                    stats.rects += 1;
                    fill_rect(t, clip, &q, tex, ppp);
                    i += 6;
                    continue;
                }
            }
            if let (Some(a), Some(b), Some(c)) =
                (v.get(idx[i] as usize), v.get(idx[i + 1] as usize), v.get(idx[i + 2] as usize))
            {
                stats.triangles += 1;
                fill_triangle(t, clip, [a, b, c], tex, ppp);
            }
            i += 3;
        }
    }
}

/// First pixel whose centre is at or right of `edge` (top-left rule).
#[inline]
fn first_px(edge: f32) -> i32 {
    (edge - 0.5).ceil() as i32
}

fn fill_rect(t: &mut Target<'_>, clip: Clip, q: &RectQuad, tex: &Texture, ppp: f32) {
    let (fx0, fy0, fx1, fy1) = (q.x0 * ppp, q.y0 * ppp, q.x1 * ppp, q.y1 * ppp);
    let xs = first_px(fx0).max(clip.x0);
    let xe = first_px(fx1).min(clip.x1);
    let ys = first_px(fy0).max(clip.y0);
    let ye = first_px(fy1).min(clip.y1);
    if xs >= xe || ys >= ye {
        return;
    }
    let c = color_of(q.color);
    let w = t.width;
    let rows = |t: &mut Target<'_>, y: i32| {
        let o = (y - t.y0) as usize * w;
        o + xs as usize..o + xe as usize
    };

    // Texel coordinates (16.16) at pixel centres, stepping per pixel.
    let tw = tex.w as f32;
    let th = tex.h as f32;
    let du = (q.u1 - q.u0) * tw / (fx1 - fx0);
    let dv = (q.v1 - q.v0) * th / (fy1 - fy0);
    let u_at = |x: i32| (q.u0 * tw + (x as f32 + 0.5 - fx0) * du) as f64;
    let v_at = |y: i32| (q.v0 * th + (y as f32 + 0.5 - fy0) * dv) as f64;

    // One texel for the whole rectangle (untextured shapes use egui's white texel).
    if du == 0.0 && dv == 0.0 || tex.w * tex.h == 1 {
        let tx = tex.sample((u_at(xs) * 65536.0) as i64, (v_at(ys) * 65536.0) as i64, TextureFilter::Nearest);
        let (r, g, b, a) = modulate(c, tx);
        if a == 0 && r == 0 && g == 0 && b == 0 {
            return;
        }
        for y in ys..ye {
            let range = rows(t, y);
            let row = &mut t.px[range];
            if a >= 255 {
                row.fill((r << 16) | (g << 8) | b);
            } else {
                for d in row {
                    *d = blend(*d, r, g, b, a);
                }
            }
        }
        return;
    }

    let filter = tex.filter(du.abs().max(dv.abs()));
    let fdu = (du as f64 * 65536.0) as i64;
    let fdv = (dv as f64 * 65536.0) as i64;
    let fu0 = (u_at(xs) * 65536.0) as i64;
    let mut fv = (v_at(ys) * 65536.0) as i64;
    // A 1:1 mapping onto texel centres (pixel-aligned text and images):
    // bilinear filtering would return the texels unchanged.
    let aligned = fdu == 0x10000 && fdv == 0x10000 && (fu0 & 0xFFFF) == 0x8000 && (fv & 0xFFFF) == 0x8000;
    let filter = if aligned { TextureFilter::Nearest } else { filter };
    let clamp_fast = aligned
        && tex.options.wrap_mode == TextureWrapMode::ClampToEdge
        && fu0 >= 0
        && fv >= 0
        && ((fu0 >> 16) + (xe - xs) as i64) <= tex.w as i64
        && ((fv >> 16) + (ye - ys) as i64) <= tex.h as i64;
    for y in ys..ye {
        let range = rows(t, y);
        let row = &mut t.px[range];
        if clamp_fast {
            let o = (fv >> 16) as usize * tex.w as usize + (fu0 >> 16) as usize;
            let src = &tex.px[o..o + row.len()];
            for (d, &s) in row.iter_mut().zip(src) {
                if s == 0 {
                    continue;
                }
                let (r, g, b, a) = modulate(c, s);
                *d = blend(*d, r, g, b, a);
            }
        } else {
            let mut fu = fu0;
            for d in row.iter_mut() {
                let s = tex.sample(fu, fv, filter);
                fu += fdu;
                if s == 0 {
                    continue;
                }
                let (r, g, b, a) = modulate(c, s);
                *d = blend(*d, r, g, b, a);
            }
        }
        fv += fdv;
    }
}

/// Sub-pixel bits of the edge functions.
const SUB: i64 = 8;

fn fill_triangle(t: &mut Target<'_>, clip: Clip, v: [&Vertex; 3], tex: &Texture, ppp: f32) {
    let one = 1i64 << SUB;
    // Clamped far outside any window, so the i64 products can't overflow.
    let fix = |p: f32| ((p * ppp).clamp(-1.0e7, 1.0e7) * one as f32).round() as i64;
    let mut p = [(fix(v[0].pos.x), fix(v[0].pos.y)), (fix(v[1].pos.x), fix(v[1].pos.y)), (fix(v[2].pos.x), fix(v[2].pos.y))];
    let mut vv = v;
    let area = (p[1].0 - p[0].0) * (p[2].1 - p[0].1) - (p[2].0 - p[0].0) * (p[1].1 - p[0].1);
    if area == 0 {
        return;
    }
    if area < 0 {
        p.swap(1, 2);
        vv.swap(1, 2);
    }

    // Bounding box in pixels, clipped.
    let minx = p.iter().map(|q| q.0).min().unwrap_or(0);
    let maxx = p.iter().map(|q| q.0).max().unwrap_or(0);
    let miny = p.iter().map(|q| q.1).min().unwrap_or(0);
    let maxy = p.iter().map(|q| q.1).max().unwrap_or(0);
    let xs = ((minx >> SUB) as i32).max(clip.x0);
    let xe = (((maxx + one - 1) >> SUB) as i32 + 1).min(clip.x1);
    let ys = ((miny >> SUB) as i32).max(clip.y0);
    let ye = (((maxy + one - 1) >> SUB) as i32 + 1).min(clip.y1);
    if xs >= xe || ys >= ye {
        return;
    }

    // Edge i runs from p[i] to p[i+1]; E(x, y) = A x + B y + C, >= 0 inside.
    // Edges that are not top or left edges lose one unit (fill rule).
    let mut ea = [0i64; 3];
    let mut eb = [0i64; 3];
    let mut ec = [0i64; 3];
    for i in 0..3 {
        let (a, b) = (p[i], p[(i + 1) % 3]);
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        ea[i] = -dy;
        eb[i] = dx;
        ec[i] = dy * a.0 - dx * a.1;
        // With y down and area > 0 in this formula (clockwise on screen), a
        // top edge is horizontal going right and a left edge goes up.
        let top_left = (dy == 0 && dx > 0) || dy < 0;
        if !top_left {
            ec[i] -= 1;
        }
    }

    // Attribute planes over pixel centres: f(x, y) = f0 + fx (x - X0) + fy (y - Y0).
    let pf = |i: usize| (vv[i].pos.x * ppp, vv[i].pos.y * ppp);
    let (x0, y0) = pf(0);
    let (x1, y1) = pf(1);
    let (x2, y2) = pf(2);
    let d = (x1 - x0) * (y2 - y0) - (x2 - x0) * (y1 - y0);
    if d == 0.0 {
        return;
    }
    let plane = |f0: f32, f1: f32, f2: f32| -> (f32, f32) {
        (((f1 - f0) * (y2 - y0) - (f2 - f0) * (y1 - y0)) / d, ((f2 - f0) * (x1 - x0) - (f1 - f0) * (x2 - x0)) / d)
    };
    let c = [color_of(vv[0].color), color_of(vv[1].color), color_of(vv[2].color)];
    let flat_color = c[0] == c[1] && c[1] == c[2];
    let flat_uv = vv[0].uv == vv[1].uv && vv[1].uv == vv[2].uv;
    let (tw, th) = (tex.w as f32, tex.h as f32);

    // Per-channel colour planes (+0.5 for rounding).
    let mut cp = [(0.0f32, 0.0f32, 0.0f32); 4];
    for (k, plane_k) in cp.iter_mut().enumerate() {
        let (gx, gy) = plane(c[0][k] as f32, c[1][k] as f32, c[2][k] as f32);
        *plane_k = (c[0][k] as f32 + 0.5, gx, gy);
    }
    let (ugx, ugy) = plane(vv[0].uv.x * tw, vv[1].uv.x * tw, vv[2].uv.x * tw);
    let (vgx, vgy) = plane(vv[0].uv.y * th, vv[1].uv.y * th, vv[2].uv.y * th);
    let filter = tex.filter(ugx.abs().max(ugy.abs()).max(vgx.abs()).max(vgy.abs()));
    let flat_texel = if flat_uv || tex.w * tex.h == 1 {
        Some(tex.sample((vv[0].uv.x * tw * 65536.0) as i64, (vv[0].uv.y * th * 65536.0) as i64, TextureFilter::Nearest))
    } else {
        None
    };

    let w = t.width;
    for y in ys..ye {
        let py = ((y as i64) << SUB) + one / 2;
        let px0 = ((xs as i64) << SUB) + one / 2;
        // The span [lo, hi) where all three edges are >= 0.
        let (mut lo, mut hi) = (xs as i64, xe as i64);
        for i in 0..3 {
            let e = ea[i] * px0 + eb[i] * py + ec[i];
            let step = ea[i] * one;
            if step == 0 {
                if e < 0 {
                    hi = lo;
                }
            } else if step > 0 {
                if e < 0 {
                    lo = lo.max(xs as i64 + (-e + step - 1) / step);
                }
            } else if e < 0 {
                hi = lo;
            } else {
                hi = hi.min(xs as i64 + e / -step + 1);
            }
        }
        if lo >= hi {
            continue;
        }
        let (lo, hi) = (lo as i32, hi as i32);
        let o = (y - t.y0) as usize * w;
        let row = &mut t.px[o + lo as usize..o + hi as usize];
        let cx = lo as f32 + 0.5 - x0;
        let cy = y as f32 + 0.5 - y0;

        if let (true, Some(tx)) = (flat_color, flat_texel) {
            let (r, g, b, a) = modulate(c[0], tx);
            if a >= 255 {
                row.fill((r << 16) | (g << 8) | b);
            } else if (r | g | b | a) != 0 {
                for d in row {
                    *d = blend(*d, r, g, b, a);
                }
            }
            continue;
        }

        let mut ch = [0f32; 4];
        for k in 0..4 {
            ch[k] = cp[k].0 + cp[k].1 * cx + cp[k].2 * cy;
        }
        let dch = [cp[0].1, cp[1].1, cp[2].1, cp[3].1];
        let mut fu = vv[0].uv.x * tw + ugx * cx + ugy * cy;
        let mut fv = vv[0].uv.y * th + vgx * cx + vgy * cy;
        for d in row.iter_mut() {
            let col = if flat_color {
                c[0]
            } else {
                [
                    (ch[0] as i32).clamp(0, 255) as u32,
                    (ch[1] as i32).clamp(0, 255) as u32,
                    (ch[2] as i32).clamp(0, 255) as u32,
                    (ch[3] as i32).clamp(0, 255) as u32,
                ]
            };
            let s = match flat_texel {
                Some(tx) => tx,
                None => tex.sample((fu as f64 * 65536.0) as i64, (fv as f64 * 65536.0) as i64, filter),
            };
            let (r, g, b, a) = modulate(col, s);
            if (r | g | b | a) != 0 {
                *d = blend(*d, r, g, b, a);
            }
            for k in 0..4 {
                ch[k] += dch[k];
            }
            fu += ugx;
            fv += vgx;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::epaint::{ColorImage, Mesh};
    use egui::{pos2, Color32, Rect};

    fn setup() -> Rasterizer {
        let mut r = Rasterizer::new();
        let img = ColorImage::new([2, 2], vec![Color32::WHITE; 4]);
        r.set_texture(TextureId::default(), &ImageDelta::full(img, TextureOptions::LINEAR));
        r
    }

    fn draw(r: &mut Rasterizer, mesh: Mesh, w: usize, h: usize) -> Vec<u32> {
        draw_stats(r, mesh, w, h).0
    }

    fn draw_stats(r: &mut Rasterizer, mesh: Mesh, w: usize, h: usize) -> (Vec<u32>, Stats) {
        let mut px = vec![0u32; w * h];
        let prim = ClippedPrimitive { clip_rect: Rect::EVERYTHING, primitive: Primitive::Mesh(mesh) };
        let st = r.paint(&mut Target { px: &mut px, width: w, height: h, y0: 0 }, &[prim], 1.0);
        (px, st)
    }

    #[test]
    fn blends_premultiplied() {
        assert_eq!(blend(0x00FF_FFFF, 0, 0, 0, 255), 0);
        assert_eq!(blend(0x0000_0000, 128, 0, 0, 128), 0x0080_0000);
        // Half-transparent black over white: grey.
        assert_eq!(blend(0x00FF_FFFF, 0, 0, 0, 128), 0x007F_7F7F);
        // Additive (alpha 0) saturates instead of wrapping.
        assert_eq!(blend(0x00F0_0000, 0x40, 0, 0, 0), 0x00FF_0000);
    }

    #[test]
    fn rect_fast_path_matches_triangles() {
        let mut r = setup();
        let mut mesh = Mesh::default();
        mesh.add_colored_rect(Rect::from_min_max(pos2(1.0, 1.0), pos2(5.0, 4.0)), Color32::RED);
        let (a, st) = draw_stats(&mut r, mesh.clone(), 8, 6);
        assert_eq!(st.rects, 1);
        // The same two triangles, one at a time.
        let r2 = setup();
        let mut px = vec![0u32; 48];
        for tri in mesh.indices.chunks(3) {
            let v = [&mesh.vertices[tri[0] as usize], &mesh.vertices[tri[1] as usize], &mesh.vertices[tri[2] as usize]];
            let tex = r2.texture(TextureId::default()).unwrap();
            fill_triangle(&mut Target { px: &mut px, width: 8, height: 6, y0: 0 }, Clip { x0: 0, y0: 0, x1: 8, y1: 6 }, v, tex, 1.0);
        }
        assert_eq!(a, px);
        assert_eq!(a.iter().filter(|&&p| p == 0x00FF_0000).count(), 12);
    }

    #[test]
    fn shared_edges_cover_once() {
        // Two half-transparent triangles sharing a diagonal: no pixel is blended twice.
        let mut r = setup();
        let mut mesh = Mesh::default();
        let c = Color32::from_rgba_premultiplied(100, 0, 0, 100);
        for p in [pos2(0.0, 0.0), pos2(7.3, 0.0), pos2(0.0, 6.0), pos2(7.3, 6.0)] {
            mesh.colored_vertex(p, c);
        }
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(1, 3, 2);
        mesh.vertices[3].color = Color32::from_rgba_premultiplied(100, 0, 0, 101); // defeat the rect path
        let px = draw(&mut r, mesh, 8, 6);
        for p in px {
            assert!(p == 0 || p == 0x0064_0000, "{p:06x}");
        }
    }

    #[test]
    fn honours_clip_rect() {
        let r = setup();
        let mut mesh = Mesh::default();
        mesh.add_colored_rect(Rect::from_min_max(pos2(0.0, 0.0), pos2(8.0, 8.0)), Color32::WHITE);
        let mut px = vec![0u32; 64];
        let prim = ClippedPrimitive {
            clip_rect: Rect::from_min_max(pos2(2.0, 2.0), pos2(4.0, 5.0)),
            primitive: Primitive::Mesh(mesh),
        };
        r.paint(&mut Target { px: &mut px, width: 8, height: 8, y0: 0 }, &[prim], 1.0);
        assert_eq!(px.iter().filter(|&&p| p != 0).count(), 6);
        assert_eq!(px[2 * 8 + 2], 0x00FF_FFFF);
    }

    #[test]
    fn samples_textures() {
        let mut r = Rasterizer::new();
        let img = ColorImage::new([2, 1], vec![Color32::RED, Color32::BLUE]);
        let id = TextureId::Managed(7);
        r.set_texture(id, &ImageDelta::full(img, TextureOptions::NEAREST));
        let mut mesh = Mesh::with_texture(id);
        mesh.add_rect_with_uv(
            Rect::from_min_max(pos2(0.0, 0.0), pos2(4.0, 1.0)),
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
        let px = draw(&mut r, mesh, 4, 1);
        assert_eq!(px, vec![0xFF0000, 0xFF0000, 0x0000FF, 0x0000FF]);
        // A partial update recolours one texel.
        let patch = ColorImage::new([1, 1], vec![Color32::GREEN]);
        r.set_texture(id, &ImageDelta::partial([1, 0], patch, TextureOptions::NEAREST));
        assert_eq!(r.texture(id).unwrap().px[1], u32::from_le_bytes(Color32::GREEN.to_array()));
    }
}
