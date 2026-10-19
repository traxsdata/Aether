//! paint.rs - software drawing into an 0x00RRGGBB pixel buffer: rectangles
//! (rounded, anti-aliased), gradients, borders, Facet text, and a page's
//! display list.

use std::collections::HashMap;
use std::sync::Arc;

use crate::css::style::{BorderStyle, DECO_LINE_THROUGH, DECO_OVERLINE, DECO_UNDERLINE};
use crate::css::Color;
use crate::dom::NodeId;
use crate::font::{Cache, Face};
use crate::image::Image;
use crate::layout::{FieldKind, Item, Page};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect { x, y, w: w.max(0), h: h.max(0) }
    }
    pub fn intersect(&self, o: &Rect) -> Rect {
        let x0 = self.x.max(o.x);
        let y0 = self.y.max(o.y);
        let x1 = (self.x + self.w).min(o.x + o.w);
        let y1 = (self.y + self.h).min(o.y + o.h);
        Rect::new(x0, y0, x1 - x0, y1 - y0)
    }
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }
}

pub struct Canvas<'a> {
    pub px: &'a mut [u32],
    pub w: i32,
    pub h: i32,
    pub pitch: i32,
    pub clip: Rect,
}

#[inline]
fn blend(dst: u32, c: Color, a: u32) -> u32 {
    if a >= 255 {
        return c.xrgb();
    }
    if a == 0 {
        return dst;
    }
    let ia = 255 - a;
    let r = (((dst >> 16) & 0xFF) * ia + c.r as u32 * a + 127) / 255;
    let g = (((dst >> 8) & 0xFF) * ia + c.g as u32 * a + 127) / 255;
    let b = ((dst & 0xFF) * ia + c.b as u32 * a + 127) / 255;
    (r << 16) | (g << 8) | b
}

pub fn mix(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color { r: l(a.r, b.r), g: l(a.g, b.g), b: l(a.b, b.b), a: l(a.a, b.a) }
}

impl<'a> Canvas<'a> {
    pub fn new(px: &'a mut [u32], w: u32, h: u32, pitch: u32) -> Canvas<'a> {
        let (w, h, pitch) = (w as i32, h as i32, pitch as i32);
        Canvas { px, w, h, pitch, clip: Rect::new(0, 0, w, h) }
    }

    /// Restricts drawing to `r` (within the current clip); returns the old clip.
    pub fn push_clip(&mut self, r: Rect) -> Rect {
        let old = self.clip;
        self.clip = self.clip.intersect(&r);
        old
    }

    #[inline]
    pub fn put(&mut self, x: i32, y: i32, c: Color, a: u32) {
        if self.clip.contains(x, y) {
            let i = (y * self.pitch + x) as usize;
            if let Some(p) = self.px.get_mut(i) {
                *p = blend(*p, c, a * c.a as u32 / 255);
            }
        }
    }

    pub fn fill(&mut self, r: Rect, c: Color) {
        if c.a == 0 {
            return;
        }
        let r = r.intersect(&self.clip);
        for y in r.y..r.y + r.h {
            let row = (y * self.pitch) as usize;
            for x in r.x..r.x + r.w {
                let p = &mut self.px[row + x as usize];
                *p = blend(*p, c, c.a as u32);
            }
        }
    }

    /// A filled rectangle with anti-aliased rounded corners (float coordinates).
    pub fn round_rect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, c: Color) {
        if c.a == 0 || w <= 0.0 || h <= 0.0 {
            return;
        }
        let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
        if r < 0.5 {
            self.fill(Rect::new(x.round() as i32, y.round() as i32, (x + w).round() as i32 - x.round() as i32, (y + h).round() as i32 - y.round() as i32), c);
            return;
        }
        let x0 = x.floor() as i32;
        let y0 = y.floor() as i32;
        let x1 = (x + w).ceil() as i32;
        let y1 = (y + h).ceil() as i32;
        let area = Rect::new(x0, y0, x1 - x0, y1 - y0).intersect(&self.clip);
        for py in area.y..area.y + area.h {
            let fy = py as f32 + 0.5;
            for px in area.x..area.x + area.w {
                let fx = px as f32 + 0.5;
                // Distance outside the rounded rectangle.
                let dx = (x + r - fx).max(fx - (x + w - r)).max(0.0);
                let dy = (y + r - fy).max(fy - (y + h - r)).max(0.0);
                let cov = if dx > 0.0 && dy > 0.0 {
                    (r + 0.5 - (dx * dx + dy * dy).sqrt()).clamp(0.0, 1.0)
                } else {
                    let ex = (fx - x + 0.5).min(x + w - fx + 0.5).clamp(0.0, 1.0);
                    let ey = (fy - y + 0.5).min(y + h - fy + 0.5).clamp(0.0, 1.0);
                    ex * ey
                };
                if cov > 0.0 {
                    self.put(px, py, c, (cov * 255.0) as u32);
                }
            }
        }
    }

    /// The outline of a rounded rectangle, `t` px thick.
    pub fn round_outline(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, t: f32, c: Color) {
        if c.a == 0 || w <= 0.0 || h <= 0.0 {
            return;
        }
        let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
        let x0 = x.floor() as i32;
        let y0 = y.floor() as i32;
        let area = Rect::new(x0, y0, (x + w).ceil() as i32 - x0, (y + h).ceil() as i32 - y0).intersect(&self.clip);
        let sd = |fx: f32, fy: f32, x: f32, y: f32, w: f32, h: f32, r: f32| -> f32 {
            // Signed distance to a rounded rect (negative inside).
            let qx = (fx - (x + w / 2.0)).abs() - (w / 2.0 - r);
            let qy = (fy - (y + h / 2.0)).abs() - (h / 2.0 - r);
            let ox = qx.max(0.0);
            let oy = qy.max(0.0);
            (ox * ox + oy * oy).sqrt() + qx.max(qy).min(0.0) - r
        };
        for py in area.y..area.y + area.h {
            for px in area.x..area.x + area.w {
                let fx = px as f32 + 0.5;
                let fy = py as f32 + 0.5;
                let d = sd(fx, fy, x, y, w, h, r);
                let outer = (0.5 - d).clamp(0.0, 1.0);
                let inner = (0.5 - (d + t)).clamp(0.0, 1.0);
                let cov = outer - inner;
                if cov > 0.0 {
                    self.put(px, py, c, (cov * 255.0) as u32);
                }
            }
        }
    }

    pub fn gradient(&mut self, r: Rect, a: Color, b: Color, vertical: bool, radius: f32) {
        let r0 = r;
        let rc = r.intersect(&self.clip);
        let len = if vertical { r0.h } else { r0.w }.max(1) as f32;
        for y in rc.y..rc.y + rc.h {
            for x in rc.x..rc.x + rc.w {
                let t = if vertical { (y - r0.y) as f32 / len } else { (x - r0.x) as f32 / len };
                let c = mix(a, b, t);
                // Rounded corners: skip pixels outside.
                if radius > 0.5 {
                    let fx = x as f32 + 0.5;
                    let fy = y as f32 + 0.5;
                    let (lx, ly, rw, rh) = (r0.x as f32, r0.y as f32, r0.w as f32, r0.h as f32);
                    let dx = (lx + radius - fx).max(fx - (lx + rw - radius)).max(0.0);
                    let dy = (ly + radius - fy).max(fy - (ly + rh - radius)).max(0.0);
                    if dx > 0.0 && dy > 0.0 {
                        let cov = (radius + 0.5 - (dx * dx + dy * dy).sqrt()).clamp(0.0, 1.0);
                        self.put(x, y, c, (cov * 255.0) as u32);
                        continue;
                    }
                }
                self.put(x, y, c, 255);
            }
        }
    }

    pub fn hline(&mut self, x0: i32, x1: i32, y: i32, c: Color) {
        self.fill(Rect::new(x0, y, x1 - x0, 1), c);
    }

    /// Draws a decoded RGBA image, scaled to fill (x, y, w, h).
    pub fn image(&mut self, x: f32, y: f32, w: f32, h: f32, img: &Image) {
        if w <= 0.0 || h <= 0.0 || img.width == 0 || img.height == 0 {
            return;
        }
        let x0 = x.floor() as i32;
        let y0 = y.floor() as i32;
        let dw = (x + w).ceil() as i32 - x0;
        let dh = (y + h).ceil() as i32 - y0;
        if dw <= 0 || dh <= 0 {
            return;
        }
        let area = Rect::new(x0, y0, dw, dh).intersect(&self.clip);
        let iw = img.width as u32;
        let ih = img.height as u32;
        for py in area.y..area.y + area.h {
            let sy = (((py - y0) as u32) * ih / dh as u32).min(ih - 1);
            let row = (sy * iw) as usize * 4;
            for px in area.x..area.x + area.w {
                let sx = (((px - x0) as u32) * iw / dw as u32).min(iw - 1);
                let o = row + sx as usize * 4;
                let a = img.pixels[o + 3];
                if a != 0 {
                    let c = Color { r: img.pixels[o], g: img.pixels[o + 1], b: img.pixels[o + 2], a: 255 };
                    self.put(px, py, c, a as u32);
                }
            }
        }
    }

    /// Draws `text` with its baseline at y. Returns the advance.
    pub fn text(&mut self, cache: &mut Cache, x: f32, y: f32, text: &str, face: Face, c: Color, letter_spacing: f32) -> f32 {
        let by = y.round() as i32;
        let mut pen = x;
        // Skip text entirely outside the clip.
        if by + (face.cell_h() as i32) < self.clip.y || by - (face.cell_h() as i32) > self.clip.y + self.clip.h {
            return face.width(text);
        }
        for ch in text.chars() {
            let adv = face.advance(ch) + letter_spacing;
            let gx = pen.round() as i32;
            if gx > self.clip.x + self.clip.w {
                pen += adv;
                continue;
            }
            if ch != ' ' && ch != '\u{A0}' {
                let g = cache.glyph(ch, face);
                let ox = gx + g.left;
                let oy = by + g.top;
                if ox + (g.w as i32) >= self.clip.x {
                    for gy in 0..g.h as i32 {
                        let row = &g.alpha[(gy as usize) * g.w as usize..(gy as usize + 1) * g.w as usize];
                        for (gx2, &a) in row.iter().enumerate() {
                            if a != 0 {
                                self.put(ox + gx2 as i32, oy + gy, c, a as u32);
                            }
                        }
                    }
                }
            }
            pen += adv;
        }
        pen - x
    }

    /// An anti-aliased polyline `t` px wide (icons in the chrome).
    pub fn stroke(&mut self, pts: &[(f32, f32)], t: f32, c: Color) {
        if pts.len() < 2 {
            return;
        }
        let r = t / 2.0;
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for &(x, y) in pts {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
        let area = Rect::new((x0 - r - 1.0).floor() as i32, (y0 - r - 1.0).floor() as i32, (x1 - x0 + t + 3.0).ceil() as i32, (y1 - y0 + t + 3.0).ceil() as i32).intersect(&self.clip);
        for py in area.y..area.y + area.h {
            for px in area.x..area.x + area.w {
                let (fx, fy) = (px as f32 + 0.5, py as f32 + 0.5);
                let mut d = f32::MAX;
                for w in pts.windows(2) {
                    let ((ax, ay), (bx, by)) = (w[0], w[1]);
                    let (vx, vy) = (bx - ax, by - ay);
                    let l2 = vx * vx + vy * vy;
                    let k = if l2 > 0.0 { (((fx - ax) * vx + (fy - ay) * vy) / l2).clamp(0.0, 1.0) } else { 0.0 };
                    let (dx, dy) = (fx - (ax + k * vx), fy - (ay + k * vy));
                    d = d.min((dx * dx + dy * dy).sqrt());
                }
                let cov = (r + 0.5 - d).clamp(0.0, 1.0);
                if cov > 0.0 {
                    self.put(px, py, c, (cov * 255.0) as u32);
                }
            }
        }
    }

    /// A small picture icon (frame, hills and a sun) for image placeholders.
    pub fn picture_icon(&mut self, x: i32, y: i32, c: Color) {
        let s = 14;
        self.round_outline(x as f32, y as f32, s as f32, (s - 2) as f32, 2.0, 1.2, c);
        for i in 0..6 {
            self.fill(Rect::new(x + 2 + i, y + 9 - i / 2, 1, 1 + i / 2), c);
            self.fill(Rect::new(x + 7 + i / 2, y + 8 - i / 3, 1, 2 + i / 3), c);
        }
        self.round_rect((x + 9) as f32, (y + 3) as f32, 2.5, 2.5, 1.2, c);
    }
}

/// Form state the painter shows: field values, checkboxes, the focused field.
#[derive(Default, Clone, Debug)]
pub struct FormState {
    pub values: HashMap<NodeId, String>,
    pub checked: HashMap<NodeId, bool>,
    pub focus: Option<NodeId>,
    pub caret_on: bool,
}

fn side_line(cv: &mut Canvas, r: Rect, horizontal: bool, style: BorderStyle, c: Color) {
    match style {
        BorderStyle::None => {}
        BorderStyle::Solid | BorderStyle::Double => cv.fill(r, c),
        BorderStyle::Dashed | BorderStyle::Dotted => {
            let (len, t) = if horizontal { (r.w, r.h.max(1)) } else { (r.h, r.w.max(1)) };
            let (on, off) = if style == BorderStyle::Dashed { (t * 3, t * 2) } else { (t, t) };
            let mut p = 0;
            while p < len {
                let seg = on.min(len - p);
                let rr = if horizontal { Rect::new(r.x + p, r.y, seg, r.h) } else { Rect::new(r.x, r.y + p, r.w, seg) };
                cv.fill(rr, c);
                p += on + off;
            }
        }
    }
}

/// Paints `page` with page point (0, scroll_y) at canvas (ox, oy), within
/// the canvas's current clip.
pub fn paint_page(cv: &mut Canvas, cache: &mut Cache, page: &Page, images: &HashMap<NodeId, Arc<Image>>, ox: i32, oy: i32, scroll_y: f32, form: &FormState) {
    let view = cv.clip;
    cv.fill(view, page.background);
    let dy = oy as f32 - scroll_y;
    let dx = ox as f32;
    let top = scroll_y - 2.0;
    let bottom = scroll_y + view.h as f32 + 2.0;
    let mut clips: Vec<Rect> = Vec::new();
    for it in &page.items {
        if let Some((a, b)) = it.y_range() {
            if b < top || a > bottom {
                continue;
            }
        }
        match it {
            Item::Deco { x, y, w, h, bg, gradient, radius, border, colors, styles } => {
                let (x, y) = (x + dx, y + dy);
                if let Some((a, b, vertical)) = gradient {
                    let r = Rect::new(x.round() as i32, y.round() as i32, w.round() as i32, h.round() as i32);
                    cv.gradient(r, *a, *b, *vertical, *radius);
                } else if !bg.is_transparent() {
                    cv.round_rect(x, y, *w, *h, *radius, *bg);
                }
                if border.iter().any(|&b| b > 0.0) {
                    let uniform = border.iter().all(|&b| b == border[0]) && colors.iter().all(|&c| c == colors[0]) && styles.iter().all(|&s| s == BorderStyle::Solid);
                    if *radius > 0.5 && uniform {
                        cv.round_outline(x, y, *w, *h, *radius, border[0], colors[0]);
                    } else {
                        let (xi, yi) = (x.round() as i32, y.round() as i32);
                        let (wi, hi) = ((x + w).round() as i32 - xi, (y + h).round() as i32 - yi);
                        let bw: Vec<i32> = border.iter().map(|b| b.round().max(if *b > 0.0 { 1.0 } else { 0.0 }) as i32).collect();
                        side_line(cv, Rect::new(xi, yi, wi, bw[0]), true, styles[0], colors[0]);
                        side_line(cv, Rect::new(xi, yi + hi - bw[2], wi, bw[2]), true, styles[2], colors[2]);
                        side_line(cv, Rect::new(xi, yi, bw[3], hi), false, styles[3], colors[3]);
                        side_line(cv, Rect::new(xi + wi - bw[1], yi, bw[1], hi), false, styles[1], colors[1]);
                    }
                }
            }
            Item::Rect { x, y, w, h, color, radius } => cv.round_rect(x + dx, y + dy, *w, *h, *radius, *color),
            Item::Text { x, y, text, face, color, deco, deco_color, letter_spacing } => {
                let (x, y) = (x + dx, y + dy);
                let adv = cv.text(cache, x, y, text, *face, *color, *letter_spacing);
                if *deco != 0 {
                    let t = (face.size as f32 / 14.0).round().max(1.0) as i32;
                    let (x0, x1) = (x.round() as i32, (x + adv).round() as i32);
                    if deco & DECO_UNDERLINE != 0 {
                        let uy = (y + face.descent() * 0.45).round() as i32;
                        cv.fill(Rect::new(x0, uy, x1 - x0, t), *deco_color);
                    }
                    if deco & DECO_LINE_THROUGH != 0 {
                        let ly = (y - face.ascent() * 0.33).round() as i32;
                        cv.fill(Rect::new(x0, ly, x1 - x0, t), *deco_color);
                    }
                    if deco & DECO_OVERLINE != 0 {
                        let oyy = (y - face.ascent() * 0.86).round() as i32;
                        cv.fill(Rect::new(x0, oyy, x1 - x0, t), *deco_color);
                    }
                }
            }
            Item::Bullet { x, y, size, kind, color } => {
                let (x, y) = (x + dx, y + dy);
                match kind {
                    1 => cv.round_outline(x, y - size / 2.0, *size, *size, size / 2.0, 1.2, *color),
                    2 => cv.round_rect(x, y - size / 2.0, *size, *size, 0.0, *color),
                    _ => cv.round_rect(x, y - size / 2.0, *size, *size, size / 2.0, *color),
                }
            }
            Item::Image { x, y, w, h, alt, node } => {
                let (x, y) = (x + dx, y + dy);
                let old = cv.push_clip(Rect::new(x.floor() as i32, y.floor() as i32, w.ceil() as i32 + 1, h.ceil() as i32 + 1));
                if let Some(img) = node.and_then(|n| images.get(&n)) {
                    cv.image(x, y, *w, *h, img);
                } else {
                    cv.round_rect(x, y, *w, *h, 4.0, Color::hex(0xEEF0F5));
                    cv.round_outline(x, y, *w, *h, 4.0, 1.0, Color::hex(0xD5D9E3));
                    let icon = Color::hex(0x9AA1B5);
                    if *h >= 16.0 && *w >= 20.0 {
                        let ix = if alt.is_empty() { x + w / 2.0 - 7.0 } else { x + 8.0 };
                        let iy = if alt.is_empty() || *h > 40.0 { y + h / 2.0 - 7.0 } else { y + (h - 12.0) / 2.0 };
                        let iy = if !alt.is_empty() && *h > 40.0 { y + 8.0 } else { iy };
                        cv.picture_icon(ix.round() as i32, iy.round() as i32, icon);
                        if !alt.is_empty() {
                            let f = Face::new(13.0, false, true, false);
                            let ty = if *h > 40.0 { y + 19.0 } else { y + h / 2.0 + 5.0 };
                            let tx = x + 28.0;
                            cv.text(cache, tx, ty, alt, f, Color::hex(0x5B6275), 0.0);
                        }
                    }
                }
                cv.clip = old;
            }
            Item::Field { x, y, w, h, node, kind, face, color } => {
                let (x, y) = (x + dx, y + dy);
                let old = cv.push_clip(Rect::new(x.floor() as i32 - 1, y.floor() as i32 - 1, w.ceil() as i32 + 2, h.ceil() as i32 + 2));
                let base = y + h / 2.0 + face.ascent() * 0.42;
                match kind {
                    FieldKind::Text { placeholder, .. } | FieldKind::TextArea { placeholder } => {
                        let password = matches!(kind, FieldKind::Text { password: true, .. });
                        let v = form.values.get(node).cloned().unwrap_or_default();
                        let base = if matches!(kind, FieldKind::TextArea { .. }) { y + face.ascent() + 2.0 } else { base };
                        let shown: String = if password { "\u{2022}".repeat(v.chars().count()) } else { v.clone() };
                        let tw = face.width(&shown);
                        // Keep the caret in view.
                        let tx = if tw > *w - 4.0 { x + w - 4.0 - tw } else { x };
                        if v.is_empty() {
                            cv.text(cache, x, base, placeholder, *face, Color::hex(0x8A90A2), 0.0);
                        } else {
                            cv.text(cache, tx, base, &shown, *face, *color, 0.0);
                        }
                        if form.focus == Some(*node) && form.caret_on {
                            let cx = (tx + if v.is_empty() { 0.0 } else { tw }).round() as i32;
                            cv.fill(Rect::new(cx + 1, (base - face.ascent()).round() as i32, 1, (face.ascent() + face.descent()).round() as i32), Color::hex(0x6D3BE8));
                        }
                    }
                    FieldKind::Button { label, .. } => {
                        let tw = face.width(label);
                        cv.text(cache, x + (w - tw) / 2.0, base, label, *face, *color, 0.0);
                    }
                    FieldKind::Check { radio, checked } => {
                        let on = form.checked.get(node).copied().unwrap_or(*checked);
                        if on {
                            let acc = Color::hex(0x6D3BE8);
                            if *radio {
                                cv.round_rect(x + w / 2.0 - 3.0, y + h / 2.0 - 3.0, 6.0, 6.0, 3.0, acc);
                            } else {
                                let f = Face::new(12.0, true, false, false);
                                cv.text(cache, x + 1.0, y + h - 2.0, "\u{2713}", f, acc, 0.0);
                            }
                        }
                    }
                    FieldKind::Select { label } => {
                        cv.text(cache, x, base, label, *face, *color, 0.0);
                        let f = Face::new(face.size as f32 * 0.7, false, false, false);
                        cv.text(cache, x + w - face.size as f32 * 0.9, base - 1.0, "\u{25BC}", f, Color::hex(0x5B6275), 0.0);
                    }
                }
                cv.clip = old;
            }
            Item::ClipPush { x, y, w, h } => {
                clips.push(cv.clip);
                let r = Rect::new((x + dx).floor() as i32, (y + dy).floor() as i32, w.ceil() as i32, h.ceil() as i32);
                cv.clip = cv.clip.intersect(&r);
            }
            Item::ClipPop => {
                if let Some(c) = clips.pop() {
                    cv.clip = c;
                }
            }
            Item::Empty | Item::Hit { .. } | Item::Anchor { .. } | Item::Region { .. } => {}
        }
    }
    cv.clip = view;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draws_within_clip() {
        let mut px = vec![0u32; 100 * 50];
        let mut cv = Canvas::new(&mut px, 100, 50, 100);
        cv.clip = Rect::new(10, 10, 20, 20);
        cv.fill(Rect::new(-100, -100, 1000, 1000), Color::WHITE);
        cv.round_rect(-50.0, -50.0, 500.0, 500.0, 30.0, Color::rgb(1, 2, 3));
        let mut cache = Cache::default();
        cv.text(&mut cache, -1e9, 1e9, "hello", Face::new(16.0, false, false, false), Color::BLACK, 0.0);
        cv.text(&mut cache, 15.0, 25.0, "hi\u{4E2D}", Face::new(300.0, true, true, false), Color::BLACK, 0.0);
        drop(cv);
        assert_eq!(px[0], 0);
        assert_eq!(px[99 + 49 * 100], 0);
        assert_ne!(px[15 + 15 * 100], 0);
    }
}
