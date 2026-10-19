//! flow.rs - block layout, tables, flex, grid and replaced elements.

use super::*;
use crate::css::style::{Align, Justify, Position, Track};
use crate::css::Len;

/// Deepest box nesting laid out; deeper boxes are skipped.
pub const MAX_LAYOUT_DEPTH: usize = 400;

pub struct Flow {
    pub items: Vec<Item>,
    pub links: Vec<String>,
    pub vw: f32,
    pub vh: f32,
    pub(crate) depth: usize,
    /// Padding (top, right, bottom, left) of the box whose content is being
    /// laid out: absolutely positioned children are placed against its
    /// padding box.
    pub(crate) pad: [f32; 4],
}

/// The result of laying out one box.
#[derive(Clone, Copy, Debug, Default)]
pub struct Laid {
    /// Border-box height.
    pub h: f32,
    /// Index of the box's Deco item, for stretching it later.
    pub deco: Option<usize>,
    /// First and last line baselines (page y), if it has lines.
    pub first_base: Option<f32>,
    pub last_base: Option<f32>,
}

pub fn layout_document(out: BuildOutput, vw: f32, vh: f32) -> Page {
    let mut f = Flow { items: Vec::new(), links: out.links, vw, vh, depth: 0, pad: [0.0; 4] };
    let root = &out.root;
    let s = &root.style;
    // The canvas takes the root's background, or else the body's.
    let mut background = s.background;
    let mut root_gradient_on_canvas = false;
    if background.is_transparent() {
        if let Some(body) = root.children.iter().find(|c| c.node.is_some() && !c.style.background.is_transparent()) {
            background = body.style.background;
        }
    } else {
        root_gradient_on_canvas = true;
    }
    if background.is_transparent() {
        background = Color::WHITE;
    }
    let (ml, mr, mt, mb) = (
        s.margin[3].or_zero(Some(vw)),
        s.margin[1].or_zero(Some(vw)),
        s.margin[0].or_zero(Some(vw)),
        s.margin[2].or_zero(Some(vw)),
    );
    let w = f.box_width(root, vw, true);
    let laid = f.layout_box(root, ml, mt, w.min(vw - ml - mr).max(0.0), vw, Some(vh));
    if root_gradient_on_canvas {
        // Don't draw the root's background twice: the canvas has it.
        if let Some(i) = laid.deco {
            if let Item::Deco { bg, .. } = &mut f.items[i] {
                *bg = Color::TRANSPARENT;
            }
        }
    }
    Page { items: f.items, width: vw, height: laid.h + mt + mb, background, links: f.links }
}

/// Horizontal box metrics resolved against containing width `cw`.
pub struct Hz {
    pub ml: Len,
    pub mr: Len,
    pub bl: f32,
    pub br: f32,
    pub pl: f32,
    pub pr: f32,
}

impl Hz {
    pub fn of(s: &Style, cw: f32) -> Hz {
        Hz {
            ml: s.margin[3],
            mr: s.margin[1],
            bl: s.border(3),
            br: s.border(1),
            pl: s.padding[3].or_zero(Some(cw)).max(0.0),
            pr: s.padding[1].or_zero(Some(cw)).max(0.0),
        }
    }
    pub fn inner(&self) -> f32 {
        self.bl + self.br + self.pl + self.pr
    }
}

fn collapse(a: f32, b: f32) -> f32 {
    if a >= 0.0 && b >= 0.0 {
        a.max(b)
    } else if a < 0.0 && b < 0.0 {
        a.min(b)
    } else {
        a + b
    }
}

/// Pixel value of a length without a containing block (percentages are 0).
fn px_only(l: Len) -> Option<f32> {
    match l {
        Len::Calc(px, pct) if pct == 0.0 => Some(px),
        _ => None,
    }
}

impl Flow {
    pub(crate) fn push(&mut self, it: Item) -> usize {
        self.items.push(it);
        self.items.len() - 1
    }

    pub(crate) fn shift_from(&mut self, start: usize, dx: f32, dy: f32) {
        if dx == 0.0 && dy == 0.0 {
            return;
        }
        for it in &mut self.items[start..] {
            it.shift(dx, dy);
        }
    }

    /// Border-box width of `b` in a containing block `cw` wide. `stretch`:
    /// an auto width fills the line (block-level boxes); otherwise it
    /// shrinks to fit the content (inline blocks, floats, tables, items).
    pub fn box_width(&mut self, b: &LBox, cw: f32, stretch: bool) -> f32 {
        let s = &b.style;
        let hz = Hz::of(s, cw);
        let margins = hz.ml.or_zero(Some(cw)) + hz.mr.or_zero(Some(cw));
        let avail = (cw - margins).max(0.0);
        let to_border = |w: f32| if s.border_box { w.max(hz.inner()) } else { w + hz.inner() };
        let mut w = match s.width.resolve(Some(cw)) {
            Some(w) => to_border(w.max(0.0)),
            None => match &b.kind {
                Kind::Replaced(_) => {
                    let (rw, _) = self.replaced_size(b, cw);
                    rw + hz.inner()
                }
                Kind::Table(_) => {
                    let (mn, mx) = self.intrinsic(b);
                    if s.width.is_auto() {
                        mx.min(avail).max(mn)
                    } else {
                        avail
                    }
                }
                _ if stretch => avail,
                _ => {
                    let (mn, mx) = self.intrinsic(b);
                    mx.min(avail.max(mn))
                }
            },
        };
        if let Some(mw) = s.max_width.resolve(Some(cw)) {
            w = w.min(to_border(mw.max(0.0)));
        }
        if let Some(mw) = s.min_width.resolve(Some(cw)) {
            w = w.max(to_border(mw.max(0.0)));
        }
        if let Kind::Table(_) = b.kind {
            // A table is never narrower than its content.
            let (mn, _) = self.intrinsic(b);
            w = w.max(mn);
        }
        w.max(hz.inner()).min(1e6)
    }

    /// (min-content, max-content) border-box widths.
    pub fn intrinsic(&mut self, b: &LBox) -> (f32, f32) {
        if let Some(c) = b.intrinsic.get() {
            return c;
        }
        if self.depth > MAX_LAYOUT_DEPTH {
            return (0.0, 0.0);
        }
        self.depth += 1;
        let s = &b.style;
        let hz = Hz::of(s, 0.0);
        let pad = hz.inner();
        let r = if let Some(w) = px_only(s.width) {
            let w = if s.border_box { w.max(pad) } else { w + pad };
            (w, w)
        } else {
            let (mn, mx) = match &b.kind {
                Kind::Replaced(_) => {
                    let (w, _) = self.replaced_size(b, 0.0);
                    let min = if s.max_width.has_pct() { 0.0 } else { w };
                    (min, w)
                }
                Kind::Block => {
                    if b.children.is_empty() {
                        let (mn, mx) = self.inline_intrinsic(&b.inlines);
                        let indent = px_only(s.indent).unwrap_or(0.0).max(0.0);
                        (mn, mx + indent)
                    } else {
                        let mut mn: f32 = 0.0;
                        let mut mx: f32 = 0.0;
                        for c in &b.children {
                            let (cmn, cmx) = self.intrinsic(c);
                            let m = px_only(c.style.margin[3]).unwrap_or(0.0) + px_only(c.style.margin[1]).unwrap_or(0.0);
                            mn = mn.max(cmn + m.max(0.0));
                            mx = mx.max(cmx + m.max(0.0));
                        }
                        (mn, mx)
                    }
                }
                Kind::Table(t) => {
                    let (cols_min, cols_max, spacing) = self.table_columns(b, t);
                    let n = cols_min.len() as f32;
                    let sp = spacing * (n + 1.0);
                    let cap = t.caption.as_ref().map(|c| self.intrinsic(c)).unwrap_or((0.0, 0.0));
                    ((cols_min.iter().sum::<f32>() + sp).max(cap.0), (cols_max.iter().sum::<f32>() + sp).max(cap.0))
                }
                Kind::Flex => {
                    let gap = px_only(s.gap.1).unwrap_or(0.0);
                    let mut mn: f32 = 0.0;
                    let mut mx: f32 = 0.0;
                    let mut sum_min = 0.0;
                    for (i, c) in b.children.iter().enumerate() {
                        let (cmn, cmx) = self.intrinsic(c);
                        let m = px_only(c.style.margin[3]).unwrap_or(0.0).max(0.0) + px_only(c.style.margin[1]).unwrap_or(0.0).max(0.0);
                        if s.flex_column {
                            mn = mn.max(cmn + m);
                            mx = mx.max(cmx + m);
                        } else {
                            mn = mn.max(cmn + m);
                            sum_min += cmn + m + if i > 0 { gap } else { 0.0 };
                            mx += cmx + m + if i > 0 { gap } else { 0.0 };
                        }
                    }
                    if !s.flex_column && !s.flex_wrap {
                        mn = mn.max(sum_min);
                    }
                    (mn, mx)
                }
                Kind::Grid => {
                    let ncols = self.grid_count(s, 1e5).max(1) as f32;
                    let gap = px_only(s.gap.1).unwrap_or(0.0);
                    let mut mn: f32 = 0.0;
                    let mut mx: f32 = 0.0;
                    for c in &b.children {
                        let (cmn, cmx) = self.intrinsic(c);
                        mn = mn.max(cmn);
                        mx = mx.max(cmx);
                    }
                    let cols = ncols.min(b.children.len().max(1) as f32);
                    (mn, mx * cols + gap * (cols - 1.0))
                }
            };
            (mn + pad, mx + pad)
        };
        let mut r = r;
        if let Some(m) = px_only(s.max_width) {
            let m = if s.border_box { m } else { m + pad };
            r = (r.0.min(m), r.1.min(m));
        }
        if let Some(m) = px_only(s.min_width) {
            let m = if s.border_box { m } else { m + pad };
            r = (r.0.max(m), r.1.max(m));
        }
        let r = (r.0.min(1e6), r.1.max(r.0).min(1e6));
        self.depth -= 1;
        b.intrinsic.set(Some(r));
        r
    }

    /// Lays out `b` with its border box at (x, y), `w` wide, in a
    /// containing block `cbw` wide (for percentages) and `cbh` tall (if known).
    pub fn layout_box(&mut self, b: &LBox, x: f32, y: f32, w: f32, cbw: f32, cbh: Option<f32>) -> Laid {
        if self.depth > MAX_LAYOUT_DEPTH {
            return Laid::default();
        }
        self.depth += 1;
        let s = &b.style;
        let start = self.items.len();
        let hz = Hz::of(s, cbw);
        let (bt, bb) = (s.border(0), s.border(2));
        let pt = s.padding[0].or_zero(Some(cbw)).max(0.0);
        let pb = s.padding[2].or_zero(Some(cbw)).max(0.0);
        let has_deco = !s.hidden
            && (!s.background.is_transparent() || s.gradient.is_some() || bt + bb + hz.bl + hz.br > 0.0 || matches!(b.kind, Kind::Replaced(_)));
        let deco = if has_deco { Some(self.push(Item::Empty)) } else { None };
        let clip = if s.overflow_hidden { Some(self.push(Item::Empty)) } else { None };
        if let Some(n) = b.node {
            self.push(Item::Anchor { y, node: n });
        }
        let region = b.node.map(|_| self.push(Item::Empty));
        let cx = x + hz.bl + hz.pl;
        let cy = y + bt + pt;
        let cw = (w - hz.inner()).max(0.0);
        let vpad = pt + pb + bt + bb;
        let spec_h = s.height.resolve(cbh).map(|h| if s.border_box { (h - vpad).max(0.0) } else { h.max(0.0) });
        let saved_pad = std::mem::replace(&mut self.pad, [pt, hz.pr, pb, hz.pl]);
        let (mut ch, first, last) = match &b.kind {
            Kind::Block => {
                if !b.children.is_empty() {
                    self.block_flow(&b.children, cx, cy, cw, spec_h)
                } else {
                    self.inline_flow(b, cx, cy, cw)
                }
            }
            Kind::Table(t) => self.table(b, t, cx, cy, cw),
            Kind::Flex => self.flex(b, cx, cy, cw, spec_h),
            Kind::Grid => self.grid(b, cx, cy, cw),
            Kind::Replaced(r) => {
                let h = self.replaced(b, r, cx, cy, cw, spec_h);
                (h, None, None)
            }
        };
        self.pad = saved_pad;
        if let Some(h) = spec_h {
            ch = h;
        }
        if let Some(m) = s.max_height.resolve(cbh) {
            let m = if s.border_box { (m - vpad).max(0.0) } else { m };
            if ch > m {
                ch = m;
            }
        }
        if let Some(m) = s.min_height.resolve(cbh) {
            let m = if s.border_box { (m - vpad).max(0.0) } else { m };
            ch = ch.max(m);
        }
        let h = ch + vpad;
        if let Some(i) = deco {
            self.items[i] = self.deco_item(s, x, y, w, h);
        }
        if let (Some(i), Some(node)) = (region, b.node) {
            self.items[i] = Item::Region { x, y, w, h, node };
        }
        if let Some(i) = clip {
            if s.overflow_hidden && (spec_h.is_some() || s.max_height.resolve(cbh).is_some()) || hz.inner() >= 0.0 {
                self.items[i] = Item::ClipPush { x: x + hz.bl, y: y + bt, w: (w - hz.bl - hz.br).max(0.0), h: (h - bt - bb).max(0.0) };
                self.push(Item::ClipPop);
            }
        }
        // List markers sit to the left of the first line.
        if let Some(m) = &b.marker {
            let base = first.unwrap_or(cy + s.font_size);
            self.marker(m, s, cx, base);
        }
        if s.position == Position::Relative {
            let dx = s.inset[3].resolve(Some(cbw)).or_else(|| s.inset[1].resolve(Some(cbw)).map(|v| -v)).unwrap_or(0.0);
            let dy = s.inset[0].resolve(cbh).or_else(|| s.inset[2].resolve(cbh).map(|v| -v)).unwrap_or(0.0);
            self.shift_from(start, dx.clamp(-2000.0, 2000.0), dy.clamp(-2000.0, 2000.0));
        }
        self.depth -= 1;
        Laid { h, deco, first_base: first, last_base: last }
    }

    pub(crate) fn deco_item(&self, s: &Style, x: f32, y: f32, w: f32, h: f32) -> Item {
        let radius = s.radius.resolve(Some(w.min(h))).unwrap_or(0.0).clamp(0.0, w.min(h) / 2.0);
        Item::Deco {
            x,
            y,
            w,
            h,
            bg: s.background,
            gradient: s.gradient,
            radius,
            border: [s.border(0), s.border(1), s.border(2), s.border(3)],
            colors: [s.border_color(0), s.border_color(1), s.border_color(2), s.border_color(3)],
            styles: s.border_style,
        }
    }

    /// Resizes a laid-out box's decoration to a new border-box height.
    pub(crate) fn set_deco_height(&mut self, deco: Option<usize>, new_h: f32) {
        if let Some(i) = deco {
            if let Some(Item::Deco { h, .. }) = self.items.get_mut(i) {
                *h = new_h;
            }
        }
    }

    fn marker(&mut self, m: &Marker, s: &Style, cx: f32, base: f32) {
        let face = crate::font::Face::new(s.font_size, false, false, false);
        match m {
            Marker::Bullet(ls) => {
                let kind = match ls {
                    crate::css::style::ListStyle::Circle => 1,
                    crate::css::style::ListStyle::Square => 2,
                    _ => 0,
                };
                let size = (s.font_size * 0.36).max(4.0);
                let x = if s.list_inside { cx } else { cx - s.font_size * 0.9 };
                self.push(Item::Bullet { x, y: base - s.font_size * 0.32, size, kind, color: s.color });
            }
            Marker::Text(t) => {
                let tw = face.width(t);
                let x = cx - tw - s.font_size * 0.45;
                self.push(Item::Text { x, y: base, text: t.clone(), face, color: s.color, deco: 0, deco_color: s.color, letter_spacing: 0.0 });
            }
        }
    }

    /// Block-level children stacked vertically, with sibling margins collapsed.
    fn block_flow(&mut self, children: &[LBox], cx: f32, cy: f32, cw: f32, cbh: Option<f32>) -> (f32, Option<f32>, Option<f32>) {
        let mut y = cy;
        let mut prev_mb: f32 = 0.0;
        let mut first = None;
        let mut last = None;
        let mut bottom_placed: Vec<(usize, usize, f32, f32, f32)> = Vec::new();
        for (i, c) in children.iter().enumerate() {
            let s = &c.style;
            if matches!(s.position, Position::Absolute | Position::Fixed) {
                // Out of flow: placed against the parent's content box (an
                // approximation of the nearest positioned ancestor), taking
                // no space. Static position when no offsets are given.
                let [pt, pr, pb, pl] = self.pad;
                let (px0, py0, pw) = (cx - pl, cy - pt, cw + pl + pr);
                let pbh = cbh.map(|h| h + pt + pb);
                let left = s.inset[3].resolve(Some(pw));
                let right = s.inset[1].resolve(Some(pw));
                let top = s.inset[0].resolve(pbh);
                let bottom = s.inset[2].resolve(pbh);
                let hz = Hz::of(s, pw);
                let cw = pw;
                let w = match (left, right) {
                    (Some(l), Some(r)) if s.width.is_auto() => (cw - l - r - hz.ml.or_zero(Some(cw)) - hz.mr.or_zero(Some(cw))).max(hz.inner()),
                    _ => self.box_width(c, cw, false),
                };
                let ml = hz.ml.or_zero(Some(cw));
                let mt = s.margin[0].or_zero(Some(cw));
                let x = match (left, right) {
                    (Some(l), _) => px0 + l + ml,
                    (None, Some(r)) => px0 + cw - r - w - hz.mr.or_zero(Some(cw)),
                    _ => cx + ml,
                };
                let static_y = y + prev_mb.max(0.0) + mt;
                let ay = match top {
                    Some(t) => py0 + t + mt,
                    None => static_y,
                };
                let start = self.items.len();
                let laid = self.layout_box(c, x.clamp(-1e5, 1e5), ay.clamp(-1e5, 1e6), w, cw, None);
                if top.is_none() {
                    if let Some(bm) = bottom {
                        bottom_placed.push((start, self.items.len(), laid.h, bm + s.margin[2].or_zero(Some(cw)) - pb, ay));
                    }
                }
                continue;
            }
            let mt = s.margin[0].or_zero(Some(cw));
            let mb = s.margin[2].or_zero(Some(cw));
            let stretch = !matches!(c.kind, Kind::Replaced(_) | Kind::Table(_));
            let w = self.box_width(c, cw, stretch);
            let hz = Hz::of(s, cw);
            // Auto margins centre (or right-align) a box narrower than the line.
            let ml = match (hz.ml, hz.mr) {
                (Len::Auto, Len::Auto) => ((cw - w) / 2.0).max(0.0),
                (Len::Auto, r) => (cw - w - r.or_zero(Some(cw))).max(0.0),
                (l, _) => l.or_zero(Some(cw)),
            };
            let gap = if i == 0 { mt } else { collapse(prev_mb, mt) };
            y += gap;
            let laid = self.layout_box(c, cx + ml, y, w, cw, cbh);
            if first.is_none() {
                first = laid.first_base;
            }
            if laid.last_base.is_some() {
                last = laid.last_base;
            }
            if laid.h == 0.0 && i > 0 {
                // An empty block: its margins collapse through it.
                y -= gap;
                prev_mb = collapse(collapse(prev_mb, mt), mb);
            } else {
                y += laid.h;
                prev_mb = mb;
            }
        }
        y += prev_mb.max(0.0);
        let total = cbh.unwrap_or(y - cy).max(0.0);
        for (start, end, h, bm, ay) in bottom_placed {
            let want = cy + total - bm - h;
            self.shift_from_range(start, end, 0.0, (want - ay).clamp(-1e5, 1e5));
        }
        ((y - cy).max(0.0), first, last)
    }

    // ---- replaced elements -------------------------------------------

    /// Content-box size of a replaced element.
    pub fn replaced_size(&mut self, b: &LBox, cw: f32) -> (f32, f32) {
        let s = &b.style;
        let Kind::Replaced(r) = &b.kind else { return (0.0, 0.0) };
        let base = if cw > 0.0 { Some(cw) } else { None };
        let hz = Hz::of(s, cw);
        let unbox = |v: f32, pad: f32| if s.border_box { (v - pad).max(0.0) } else { v };
        let sw = s.width.resolve(base).map(|v| unbox(v, hz.inner()));
        let vpad = s.padding[0].or_zero(base) + s.padding[2].or_zero(base) + s.border(0) + s.border(2);
        let sh = s.height.resolve(None).map(|v| unbox(v, vpad));
        let fs = s.font_size;
        let face = s.face();
        let (mut w, mut h) = match r {
            Replaced::Image { alt, .. } => {
                let intrinsic = b.intrinsic.get();
                match (sw, sh) {
                    (Some(w), Some(h)) => (w, h),
                    (Some(w), None) => {
                        let h = intrinsic.map(|(iw, ih)| if iw > 0.0 { (w * ih / iw).round() } else { (w * 0.62).round() }).unwrap_or((w * 0.62).round());
                        (w, h)
                    }
                    (None, Some(h)) => {
                        let w = intrinsic.map(|(iw, ih)| if ih > 0.0 { (h * iw / ih).round() } else { (h * 1.6).round() }).unwrap_or((h * 1.6).round());
                        (w, h)
                    }
                    (None, None) => {
                        if let Some((w, h)) = intrinsic {
                            (w, h)
                        } else if alt.trim().is_empty() {
                            (0.0, 0.0)
                        } else {
                            let f = crate::font::Face::new(fs.min(14.0), false, true, false);
                            let t = crate::font::normalize(alt.trim());
                            ((f.width(&t) + 30.0).min(400.0).min(if cw > 0.0 { cw } else { 400.0 }), (fs.min(14.0) * 1.3 + 8.0).round())
                        }
                    }
                }
            }
            Replaced::Frame { .. } => (sw.unwrap_or(300.0), sh.unwrap_or(150.0)),
            Replaced::Blank => (sw.unwrap_or(0.0), sh.unwrap_or(0.0)),
            Replaced::Field(k) => {
                let line = (fs * 1.3).round();
                match k {
                    FieldKind::Text { .. } => (sw.unwrap_or(fs * 13.0), sh.unwrap_or(line)),
                    FieldKind::TextArea { .. } => (sw.unwrap_or(fs * 16.0), sh.unwrap_or(line * 3.0)),
                    FieldKind::Button { label, .. } => (sw.unwrap_or(face.width(&crate::font::normalize(label)).ceil()), sh.unwrap_or(line)),
                    FieldKind::Check { .. } => (sw.unwrap_or(13.0), sh.unwrap_or(13.0)),
                    FieldKind::Select { label } => (sw.unwrap_or(face.width(&crate::font::normalize(label)).ceil() + fs * 1.6), sh.unwrap_or(line)),
                }
            }
        };
        // CSS max/min sizing applies to pictures too. The common responsive
        // rule is `img { max-width: 100%; height: auto }`; preserve the
        // aspect ratio when a constraint changes one side.
        if matches!(r, Replaced::Image { .. }) {
            if let Some(max) = s.max_width.resolve(base) {
                if w > max && w > 0.0 {
                    let scale = max / w;
                    w = max;
                    h = (h * scale).round();
                }
            }
            if let Some(max) = s.max_height.resolve(Some(h)) {
                if h > max && h > 0.0 {
                    let scale = max / h;
                    h = max;
                    w = (w * scale).round();
                }
            }
            if let Some(min) = s.min_width.resolve(base) {
                if w < min {
                    let scale = if w > 0.0 { min / w } else { 1.0 };
                    w = min;
                    h = (h * scale).round();
                }
            }
        }
        (w, h)
    }

    fn replaced(&mut self, b: &LBox, r: &Replaced, cx: f32, cy: f32, cw: f32, spec_h: Option<f32>) -> f32 {
        let (_, h0) = self.replaced_size(b, cw);
        let h = spec_h.unwrap_or(h0);
        let s = &b.style;
        if s.hidden {
            return h;
        }
        match r {
            Replaced::Image { alt, .. } => {
                if cw > 0.0 && h > 0.0 {
                    self.push(Item::Image { x: cx, y: cy, w: cw, h, alt: crate::font::normalize(alt.trim()), node: b.node });
                }
            }
            Replaced::Frame { src } => {
                let face = crate::font::Face::new(13.0, false, false, false);
                self.push(Item::Rect { x: cx, y: cy, w: cw, h, color: Color::hex(0xF1F2F7), radius: 4.0 });
                let label = "Embedded content isn't loaded";
                self.push(Item::Text { x: cx + 10.0, y: cy + 22.0, text: label.into(), face, color: Color::hex(0x5B6275), deco: 0, deco_color: Color::BLACK, letter_spacing: 0.0 });
                if !src.is_empty() && h > 44.0 {
                    let link = self.links.len() as u32;
                    self.links.push(src.clone());
                    let t = "Open it on its own page";
                    let tw = face.width(t);
                    self.push(Item::Text { x: cx + 10.0, y: cy + 42.0, text: t.into(), face, color: Color::hex(0x1D4ED8), deco: crate::css::style::DECO_UNDERLINE, deco_color: Color::hex(0x1D4ED8), letter_spacing: 0.0 });
                    self.push(Item::Hit { x: cx + 10.0, y: cy + 28.0, w: tw, h: 18.0, target: Target::Link(link) });
                }
            }
            Replaced::Blank => {}
            Replaced::Field(k) => {
                let node = b.node.unwrap_or(0);
                self.push(Item::Field { x: cx, y: cy, w: cw, h, node, kind: k.clone(), face: s.face(), color: s.color });
                let p = b.style.padding.map(|p| p.or_zero(Some(cw)));
                let target = match k {
                    FieldKind::Text { .. } | FieldKind::TextArea { .. } => Some(Target::Field(node)),
                    FieldKind::Button { submit: true, .. } => Some(Target::Submit(node)),
                    FieldKind::Check { .. } => Some(Target::Check(node)),
                    _ => None,
                };
                if let Some(t) = target {
                    self.push(Item::Hit { x: cx - p[3], y: cy - p[0], w: cw + p[1] + p[3], h: h + p[0] + p[2], target: t });
                }
            }
        }
        h
    }

    // ---- tables -------------------------------------------------------

    /// Places cells on the grid: (row, column) of every cell.
    fn table_grid(t: &TableData) -> (Vec<Vec<(usize, usize)>>, usize) {
        let mut occupied: Vec<Vec<bool>> = Vec::new();
        let mut pos = Vec::new();
        let mut ncols = 0;
        for (r, row) in t.rows.iter().enumerate() {
            let mut rp = Vec::new();
            let mut col = 0;
            for cell in &row.cells {
                while occupied.get(r).and_then(|o| o.get(col)).copied().unwrap_or(false) {
                    col += 1;
                }
                if col >= 512 {
                    break;
                }
                let span = cell.colspan.min(512 - col);
                let rspan = cell.rowspan.min(t.rows.len() - r).max(1);
                for rr in r..r + rspan {
                    if occupied.len() <= rr {
                        occupied.resize(rr + 1, Vec::new());
                    }
                    let o = &mut occupied[rr];
                    if o.len() < col + span {
                        o.resize(col + span, false);
                    }
                    for c in col..col + span {
                        o[c] = true;
                    }
                }
                rp.push((col, span));
                col += span;
                ncols = ncols.max(col);
            }
            pos.push(rp);
        }
        (pos, ncols)
    }

    /// Column min/max widths and the spacing between cells.
    fn table_columns(&mut self, b: &LBox, t: &TableData) -> (Vec<f32>, Vec<f32>, f32) {
        let s = &b.style;
        let spacing = if s.collapse { 0.0 } else { s.spacing };
        let (pos, ncols) = Self::table_grid(t);
        let mut mn = vec![0.0f32; ncols];
        let mut mx = vec![0.0f32; ncols];
        let mut spans = Vec::new();
        for (r, row) in t.rows.iter().enumerate() {
            for (i, cell) in row.cells.iter().enumerate() {
                let Some(&(col, span)) = pos[r].get(i) else { continue };
                let (cmn, cmx) = self.intrinsic(&cell.b);
                if span == 1 {
                    mn[col] = mn[col].max(cmn);
                    mx[col] = mx[col].max(cmx);
                } else {
                    spans.push((col, span, cmn, cmx));
                }
            }
        }
        for (col, span, cmn, cmx) in spans {
            let end = (col + span).min(ncols);
            let have_mn: f32 = mn[col..end].iter().sum::<f32>() + spacing * (span - 1) as f32;
            let have_mx: f32 = mx[col..end].iter().sum::<f32>() + spacing * (span - 1) as f32;
            let n = (end - col).max(1) as f32;
            if cmn > have_mn {
                for c in &mut mn[col..end] {
                    *c += (cmn - have_mn) / n;
                }
            }
            if cmx > have_mx {
                for c in &mut mx[col..end] {
                    *c += (cmx - have_mx) / n;
                }
            }
        }
        for i in 0..ncols {
            mx[i] = mx[i].max(mn[i]);
        }
        (mn, mx, spacing)
    }

    fn table(&mut self, b: &LBox, t: &TableData, cx: f32, cy: f32, cw: f32) -> (f32, Option<f32>, Option<f32>) {
        let mut y = cy;
        let mut first = None;
        if let Some(cap) = &t.caption {
            let laid = self.layout_box(cap, cx, y, cw, cw, None);
            y += laid.h;
            first = laid.first_base;
        }
        let (mn, mx, spacing) = self.table_columns(b, t);
        let ncols = mn.len();
        if ncols == 0 {
            return (y - cy, first, first);
        }
        let avail = (cw - spacing * (ncols as f32 + 1.0)).max(0.0);
        let sum_mn: f32 = mn.iter().sum();
        let sum_mx: f32 = mx.iter().sum();
        let widths: Vec<f32> = if sum_mx <= avail {
            let extra = avail - sum_mx;
            if b.style.width.is_auto() && extra > 0.5 {
                mx.clone()
            } else if sum_mx > 0.0 {
                mx.iter().map(|w| w + extra * w / sum_mx).collect()
            } else {
                vec![avail / ncols as f32; ncols]
            }
        } else if sum_mn <= avail {
            let range = (sum_mx - sum_mn).max(0.001);
            mn.iter().zip(mx.iter()).map(|(a, b)| a + (avail - sum_mn) * (b - a) / range).collect()
        } else {
            mn.clone()
        };
        let mut col_x = Vec::with_capacity(ncols + 1);
        let mut x = cx + spacing;
        for w in &widths {
            col_x.push(x);
            x += w + spacing;
        }
        col_x.push(x);
        let (pos, _) = Self::table_grid(t);
        y += spacing;
        let nrows = t.rows.len();
        let mut row_y = vec![0.0f32; nrows + 1];
        let mut row_h = vec![0.0f32; nrows];
        // (row, laid, content start index, rowspan, valign)
        let mut placed: Vec<(usize, Laid, usize, usize, crate::css::style::VAlign)> = Vec::new();
        let mut pending_spans: Vec<(usize, f32)> = Vec::new(); // (last row, needed height from start row y)
        let mut row_slots = Vec::new();
        for (r, row) in t.rows.iter().enumerate() {
            row_y[r] = y;
            let slot = if !row.style.background.is_transparent() { Some(self.push(Item::Empty)) } else { None };
            row_slots.push(slot);
            let mut h: f32 = row.style.height.resolve(None).unwrap_or(0.0);
            for (i, cell) in row.cells.iter().enumerate() {
                let Some(&(col, span)) = pos[r].get(i) else { continue };
                let end = (col + span).min(ncols);
                let w = col_x[end] - spacing - col_x[col];
                let start = self.items.len();
                let laid = self.layout_box(&cell.b, col_x[col], y, w.max(0.0), cw, None);
                let rspan = cell.rowspan.min(nrows - r).max(1);
                if rspan == 1 {
                    h = h.max(laid.h);
                } else {
                    pending_spans.push((r + rspan - 1, laid.h));
                }
                if first.is_none() {
                    first = laid.first_base;
                }
                placed.push((r, laid, start, rspan, cell.b.style.cell_valign));
            }
            // Spanning cells that end on this row: make the rows tall enough.
            let mut need: f32 = 0.0;
            for &(last, ch) in &pending_spans {
                if last == r {
                    need = need.max(ch);
                }
            }
            if need > 0.0 {
                // The span started at some earlier row: compare with the total.
                for (pr, laid, _, rspan, _) in &placed {
                    if pr + rspan - 1 == r && *rspan > 1 {
                        let span_h = y + h - row_y[*pr];
                        if laid.h > span_h {
                            h += laid.h - span_h;
                        }
                    }
                }
            }
            row_h[r] = h;
            y += h + spacing;
        }
        row_y[nrows] = y;
        // Stretch cells to their rows and align their content vertically.
        // Each cell's items run until the next cell's start.
        let starts: Vec<usize> = placed.iter().map(|p| p.2).collect();
        let ends: Vec<usize> = placed
            .iter()
            .map(|p| starts.iter().copied().filter(|&s| s > p.2).min().unwrap_or(self.items.len()))
            .collect();
        for (k, (r, laid, start, rspan, valign)) in placed.iter().enumerate() {
            let full = row_y[r + rspan] - spacing - row_y[*r];
            let extra = full - laid.h;
            if extra > 0.5 {
                self.set_deco_height(laid.deco, full);
                let dy = match valign {
                    crate::css::style::VAlign::Middle => extra / 2.0,
                    crate::css::style::VAlign::Bottom => extra,
                    _ => 0.0,
                };
                if dy > 0.0 {
                    let from = laid.deco.map_or(*start, |d| d + 1);
                    let to = ends[k];
                    for it in &mut self.items[from..to] {
                        if !matches!(it, Item::Anchor { .. }) {
                            it.shift(0.0, dy);
                        }
                    }
                }
            }
        }
        for (r, slot) in row_slots.into_iter().enumerate() {
            if let Some(i) = slot {
                self.items[i] = Item::Rect { x: cx + spacing, y: row_y[r], w: (col_x[ncols] - cx - 2.0 * spacing).max(0.0), h: row_h[r], color: t.rows[r].style.background, radius: 0.0 };
            }
        }
        (y - cy, first, first)
    }

    // ---- flex -----------------------------------------------------------

    fn flex(&mut self, b: &LBox, cx: f32, cy: f32, cw: f32, spec_h: Option<f32>) -> (f32, Option<f32>, Option<f32>) {
        let s = &b.style;
        let gap_r = s.gap.0.or_zero(Some(cw)).max(0.0);
        let gap_c = s.gap.1.or_zero(Some(cw)).max(0.0);
        if s.flex_column {
            return self.flex_column(b, cx, cy, cw, gap_r);
        }
        struct It {
            hypo: f32,
            min: f32,
            max: f32,
            ml: f32,
            mr: f32,
            ml_auto: bool,
            mr_auto: bool,
            grow: f32,
            shrink: f32,
            base: f32,
        }
        let mut its: Vec<It> = Vec::new();
        for c in &b.children {
            let cs = &c.style;
            let hz = Hz::of(cs, cw);
            let (imn, imx) = self.intrinsic(c);
            let to_border = |w: f32| if cs.border_box { w.max(hz.inner()) } else { w + hz.inner() };
            let basis = match cs.basis.resolve(Some(cw)) {
                Some(v) => to_border(v.max(0.0)),
                None => match cs.width.resolve(Some(cw)) {
                    Some(w) => to_border(w.max(0.0)),
                    None => imx,
                },
            };
            let maxw = cs.max_width.resolve(Some(cw)).map(to_border).unwrap_or(f32::MAX);
            let minw = cs.min_width.resolve(Some(cw)).map(to_border).unwrap_or(if cs.width.is_auto() && !cs.overflow_hidden { imn.min(maxw) } else { 0.0 });
            its.push(It {
                hypo: basis.clamp(minw.min(maxw), maxw),
                min: minw,
                max: maxw,
                ml: hz.ml.or_zero(Some(cw)),
                mr: hz.mr.or_zero(Some(cw)),
                ml_auto: hz.ml.is_auto(),
                mr_auto: hz.mr.is_auto(),
                grow: cs.grow,
                shrink: cs.shrink,
                base: basis.min(maxw),
            });
        }
        // Lines.
        let mut lines: Vec<(usize, usize)> = Vec::new();
        let mut start = 0;
        let mut used = 0.0;
        for (i, it) in its.iter().enumerate() {
            let outer = it.hypo + it.ml + it.mr;
            if s.flex_wrap && i > start && used + gap_c + outer > cw {
                lines.push((start, i));
                start = i;
                used = 0.0;
            }
            used += outer + if i > start { gap_c } else { 0.0 };
        }
        if start < its.len() {
            lines.push((start, its.len()));
        }
        let mut y = cy;
        let mut first = None;
        let mut last = None;
        let single = lines.len() == 1;
        let rev = s.flex_reverse;
        for (li, &(a, z)) in lines.iter().enumerate() {
            let n = z - a;
            let mut sizes: Vec<f32> = its[a..z].iter().map(|i| i.hypo).collect();
            let margins: f32 = its[a..z].iter().map(|i| i.ml + i.mr).sum();
            let gaps = gap_c * (n.saturating_sub(1)) as f32;
            let free = cw - sizes.iter().sum::<f32>() - margins - gaps;
            if free > 0.0 {
                let total_grow: f32 = its[a..z].iter().map(|i| i.grow).sum();
                if total_grow > 0.0 {
                    // Grow from the flex base sizes, then respect min/max.
                    let base_free = cw - its[a..z].iter().map(|i| if i.grow > 0.0 { i.base } else { i.hypo }).sum::<f32>() - margins - gaps;
                    for (k, it) in its[a..z].iter().enumerate() {
                        if it.grow > 0.0 {
                            sizes[k] = (it.base + base_free.max(0.0) * it.grow / total_grow).clamp(it.min.min(it.max), it.max);
                        }
                    }
                }
            } else if free < 0.0 {
                let weights: Vec<f32> = its[a..z].iter().zip(&sizes).map(|(i, s)| i.shrink * s).collect();
                let tw: f32 = weights.iter().sum();
                if tw > 0.0 {
                    for (k, it) in its[a..z].iter().enumerate() {
                        sizes[k] = (sizes[k] + free * weights[k] / tw).max(it.min).max(0.0);
                    }
                }
            }
            let mut rem = cw - sizes.iter().sum::<f32>() - margins - gaps;
            let autos = its[a..z].iter().map(|i| i.ml_auto as u32 + i.mr_auto as u32).sum::<u32>();
            let auto_m = if autos > 0 && rem > 0.0 {
                let m = rem / autos as f32;
                rem = 0.0;
                m
            } else {
                0.0
            };
            let rem = rem.max(0.0);
            let (mut x, between) = match (s.justify, n) {
                (Justify::Start, _) => (0.0, 0.0),
                (Justify::End, _) => (rem, 0.0),
                (Justify::Center, _) => (rem / 2.0, 0.0),
                (Justify::SpaceBetween, 1) => (0.0, 0.0),
                (Justify::SpaceBetween, _) => (0.0, rem / (n - 1) as f32),
                (Justify::SpaceAround, _) => (rem / n as f32 / 2.0, rem / n as f32),
                (Justify::SpaceEvenly, _) => (rem / (n + 1) as f32, rem / (n + 1) as f32),
            };
            if rev {
                x = rem - x;
            }
            // Lay out each item at its main position.
            let mut laid_items: Vec<(Laid, usize, usize, f32, f32)> = Vec::new(); // (laid, start, end, mt, mb)
            let order: Vec<usize> = if rev { (a..z).rev().collect() } else { (a..z).collect() };
            for (k, &i) in order.iter().enumerate() {
                let it = &its[i];
                let c = &b.children[i];
                let ml = if it.ml_auto { auto_m } else { it.ml };
                let mr = if it.mr_auto { auto_m } else { it.mr };
                let mt = c.style.margin[0].or_zero(Some(cw));
                let mb = c.style.margin[2].or_zero(Some(cw));
                let w = sizes[i - a];
                let start = self.items.len();
                let laid = self.layout_box(c, cx + x + ml, y + mt, w, cw, None);
                if first.is_none() {
                    first = laid.first_base;
                }
                if laid.last_base.is_some() {
                    last = laid.last_base;
                }
                laid_items.push((laid, start, self.items.len(), mt, mb));
                x += ml + w + mr + if k + 1 < n { gap_c + between } else { 0.0 };
            }
            let mut line_h = laid_items.iter().map(|l| l.0.h + l.3 + l.4).fold(0.0f32, f32::max);
            if single {
                if let Some(h) = spec_h {
                    line_h = line_h.max(h);
                }
            }
            for (k, (laid, st, en, mt, mb)) in laid_items.iter().enumerate() {
                let i = order[k];
                let c = &b.children[i];
                let room = line_h - laid.h - mt - mb;
                if room <= 0.0 {
                    continue;
                }
                match s.align_items {
                    Align::Stretch if c.style.height.is_auto() => self.set_deco_height(laid.deco, laid.h + room),
                    Align::Center => self.shift_from_range(*st, *en, 0.0, room / 2.0),
                    Align::End => self.shift_from_range(*st, *en, 0.0, room),
                    _ => {}
                }
            }
            y += line_h;
            if li + 1 < lines.len() {
                y += gap_r;
            }
        }
        (y - cy, first, last)
    }

    pub(crate) fn shift_from_range(&mut self, start: usize, end: usize, dx: f32, dy: f32) {
        let end = end.min(self.items.len());
        for it in &mut self.items[start..end] {
            it.shift(dx, dy);
        }
    }

    fn flex_column(&mut self, b: &LBox, cx: f32, cy: f32, cw: f32, gap: f32) -> (f32, Option<f32>, Option<f32>) {
        let s = &b.style;
        let mut y = cy;
        let mut first = None;
        let mut last = None;
        let n = b.children.len();
        for (i, c) in b.children.iter().enumerate() {
            let cs = &c.style;
            let stretch = s.align_items == Align::Stretch && cs.width.is_auto() && !matches!(c.kind, Kind::Replaced(_));
            let w = self.box_width(c, cw, stretch);
            let hz = Hz::of(cs, cw);
            let ml = hz.ml.or_zero(Some(cw));
            let mr = hz.mr.or_zero(Some(cw));
            let x = match (s.align_items, hz.ml.is_auto(), hz.mr.is_auto()) {
                (_, true, true) => ((cw - w) / 2.0).max(0.0),
                (_, true, false) => (cw - w - mr).max(0.0),
                (Align::Center, _, _) => ((cw - w - ml - mr) / 2.0).max(0.0) + ml,
                (Align::End, _, _) => (cw - w - mr).max(0.0),
                _ => ml,
            };
            let mt = cs.margin[0].or_zero(Some(cw));
            let mb = cs.margin[2].or_zero(Some(cw));
            y += mt;
            let laid = self.layout_box(c, cx + x, y, w, cw, None);
            if first.is_none() {
                first = laid.first_base;
            }
            if laid.last_base.is_some() {
                last = laid.last_base;
            }
            y += laid.h + mb + if i + 1 < n { gap } else { 0.0 };
        }
        (y - cy, first, last)
    }

    // ---- grid -----------------------------------------------------------

    fn grid_count(&self, s: &Style, cw: f32) -> usize {
        let gap = s.gap.1.or_zero(Some(cw)).max(0.0);
        let mut n = 0;
        for t in &s.grid_columns {
            n += match t {
                Track::AutoFill(min) => (((cw + gap) / (min + gap)).floor() as usize).clamp(1, 64),
                _ => 1,
            };
        }
        n.max(1)
    }

    fn grid(&mut self, b: &LBox, cx: f32, cy: f32, cw: f32) -> (f32, Option<f32>, Option<f32>) {
        let s = &b.style;
        let gap_r = s.gap.0.or_zero(Some(cw)).max(0.0);
        let gap_c = s.gap.1.or_zero(Some(cw)).max(0.0);
        // Column widths.
        let mut tracks: Vec<Track> = Vec::new();
        for t in &s.grid_columns {
            match t {
                Track::AutoFill(min) => {
                    let n = (((cw + gap_c) / (min + gap_c)).floor() as usize).clamp(1, 64);
                    tracks.extend(std::iter::repeat_n(Track::Fr(1.0), n));
                }
                t => tracks.push(*t),
            }
        }
        if tracks.is_empty() {
            tracks.push(Track::Fr(1.0));
        }
        let n = tracks.len();
        let avail = (cw - gap_c * (n - 1) as f32).max(0.0);
        let fixed: f32 = tracks.iter().map(|t| if let Track::Len(l) = t { l.or_zero(Some(cw)).max(0.0) } else { 0.0 }).sum();
        let frs: f32 = tracks.iter().map(|t| match t {
            Track::Fr(f) => *f,
            Track::Auto => 1.0,
            _ => 0.0,
        }).sum();
        let left = (avail - fixed).max(0.0);
        let widths: Vec<f32> = tracks
            .iter()
            .map(|t| match t {
                Track::Len(l) => l.or_zero(Some(cw)).max(0.0),
                Track::Fr(f) if frs > 0.0 => left * f / frs,
                Track::Auto if frs > 0.0 => left / frs,
                _ => 0.0,
            })
            .collect();
        let mut xs = Vec::with_capacity(n + 1);
        let mut x = 0.0;
        for w in &widths {
            xs.push(x);
            x += w + gap_c;
        }
        xs.push(x);
        let mut y = cy;
        let mut first = None;
        let mut last = None;
        let mut col = 0;
        let mut row: Vec<(Laid, usize, usize, bool)> = Vec::new();
        let mut row_h: f32 = 0.0;
        let finish_row = |f: &mut Flow, row: &mut Vec<(Laid, usize, usize, bool)>, row_h: f32| {
            for (laid, _, _, stretch) in row.drain(..) {
                if stretch && laid.h < row_h {
                    f.set_deco_height(laid.deco, row_h);
                }
            }
        };
        for c in &b.children {
            let span = (c.style.grid_span as usize).clamp(1, n);
            if col + span > n && col > 0 {
                finish_row(self, &mut row, row_h);
                y += row_h + gap_r;
                row_h = 0.0;
                col = 0;
            }
            let w = xs[col + span] - gap_c - xs[col];
            let mt = c.style.margin[0].or_zero(Some(cw));
            let mb = c.style.margin[2].or_zero(Some(cw));
            let iw = if c.style.width.is_auto() { w } else { self.box_width(c, w, true).min(w) };
            let start = self.items.len();
            let laid = self.layout_box(c, cx + xs[col], y + mt, iw, w, None);
            if first.is_none() {
                first = laid.first_base;
            }
            if laid.last_base.is_some() {
                last = laid.last_base;
            }
            row_h = row_h.max(laid.h + mt + mb);
            row.push((laid, start, self.items.len(), c.style.height.is_auto()));
            col += span;
            if col >= n {
                finish_row(self, &mut row, row_h);
                y += row_h + gap_r;
                row_h = 0.0;
                col = 0;
            }
        }
        if col > 0 {
            finish_row(self, &mut row, row_h);
            y += row_h;
        } else if !b.children.is_empty() {
            y -= gap_r;
        }
        ((y - cy).max(0.0), first, last)
    }
}
