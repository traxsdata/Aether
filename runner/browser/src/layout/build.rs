//! build.rs - the box tree: which boxes the DOM generates, given styles.

use std::rc::Rc;

use super::*;
use crate::css::style::{Display, Float, ListStyle, TextTransform, VAlign};
use crate::css::Styles;
use crate::dom::{Document, NodeData};

pub const MAX_BOXES: usize = 120_000;

pub struct BuildOutput {
    pub root: LBox,
    pub links: Vec<String>,
}

/// What inline content inherits from its ancestors.
#[derive(Clone, Copy, Debug)]
struct Ctx {
    deco: u8,
    deco_color: Option<Color>,
    link: Option<u32>,
    shift: f32,
    bg: Option<Color>,
    /// The innermost inline element (for scripts' hit testing).
    elem: Option<NodeId>,
}

const NO_CTX: Ctx = Ctx { deco: 0, deco_color: None, link: None, shift: 0.0, bg: None, elem: None };

struct Builder<'a> {
    doc: &'a Document,
    styles: &'a Styles,
    links: Vec<String>,
    blockish: Vec<Option<bool>>,
    boxes: usize,
}

pub fn build_tree(doc: &Document, styles: &Styles) -> BuildOutput {
    let mut b = Builder { doc, styles, links: Vec::new(), blockish: vec![None; doc.nodes.len()], boxes: 0 };
    let root = match doc.html_element().and_then(|h| styles.get(h).map(|s| (h, s.clone()))) {
        Some((h, s)) if s.display != Display::None => {
            let mut s2 = (*s).clone();
            s2.display = Display::Block;
            b.build_box(h, Rc::new(s2), NO_CTX)
        }
        _ => LBox::new(Rc::new(Style::initial()), None, Kind::Block),
    };
    BuildOutput { root, links: b.links }
}

/// Elements drawn by the browser itself rather than from their children.
fn replaced_kind(doc: &Document, n: NodeId) -> Option<Replaced> {
    let e = doc.element(n)?;
    if e.foreign {
        return Some(Replaced::Blank);
    }
    let attr = |k: &str| e.attr(k).unwrap_or("").to_string();
    Some(match e.name.as_str() {
        "img" => Replaced::Image { alt: attr("alt"), src: attr("src") },
        "video" | "audio" | "canvas" => Replaced::Image { alt: String::new(), src: String::new() },
        "iframe" | "embed" | "frame" => Replaced::Frame { src: attr("src") },
        "object" => Replaced::Frame { src: attr("data") },
        "input" => {
            let t = e.attr("type").unwrap_or("text").to_ascii_lowercase();
            Replaced::Field(match t.as_str() {
                "submit" | "button" | "reset" | "image" => FieldKind::Button {
                    label: e.attr("value").map(|s| s.to_string()).unwrap_or_else(|| match t.as_str() {
                        "reset" => "Reset".into(),
                        "button" => String::new(),
                        _ => "Submit".into(),
                    }),
                    submit: t == "submit" || t == "image",
                },
                "checkbox" | "radio" => FieldKind::Check { radio: t == "radio", checked: e.attr("checked").is_some() },
                "file" => FieldKind::Button { label: "Choose file (not available)".into(), submit: false },
                "range" | "color" => return Some(Replaced::Blank),
                _ => FieldKind::Text { placeholder: attr("placeholder"), password: t == "password" },
            })
        }
        "textarea" => Replaced::Field(FieldKind::TextArea { placeholder: attr("placeholder") }),
        "select" => {
            // The selected option's text (or the first).
            let opts: Vec<NodeId> = doc.descendants(n).into_iter().filter(|&o| doc.is(o, "option")).collect();
            let sel = opts.iter().copied().find(|&o| doc.element(o).is_some_and(|e| e.attr("selected").is_some())).or(opts.first().copied());
            let label = sel.map(|o| doc.text_content(o).split_whitespace().collect::<Vec<_>>().join(" ")).unwrap_or_default();
            Replaced::Field(FieldKind::Select { label })
        }
        _ => return None,
    })
}

impl Builder<'_> {
    fn style(&self, n: NodeId) -> Option<Rc<Style>> {
        self.styles.get(n).cloned()
    }

    fn shown(&self, n: NodeId) -> Option<Rc<Style>> {
        let s = self.style(n)?;
        if s.display == Display::None || s.is_offscreen() {
            return None;
        }
        Some(s)
    }

    /// Children, with display:contents elements replaced by their children.
    fn flat_children(&self, n: NodeId, out: &mut Vec<NodeId>, depth: usize) {
        for &c in self.doc.children(n) {
            if depth < 32 && self.doc.element(c).is_some() && self.style(c).is_some_and(|s| s.display == Display::Contents) {
                self.flat_children(c, out, depth + 1);
            } else {
                out.push(c);
            }
        }
    }

    fn is_atomic(&self, n: NodeId, s: &Style) -> bool {
        matches!(s.display, Display::InlineBlock | Display::InlineTable | Display::InlineFlex | Display::InlineGrid)
            || replaced_kind(self.doc, n).is_some()
            || s.is_float()
    }

    /// Does this element (an inline one) contain block-level content? Then
    /// it is laid out as a block (the simple form of "block-in-inline").
    fn blockish(&mut self, n: NodeId, depth: usize) -> bool {
        if let Some(b) = self.blockish[n] {
            return b;
        }
        let Some(s) = self.shown(n) else { return false };
        let r = if self.is_atomic(n, &s) {
            false
        } else if !matches!(s.display, Display::Inline | Display::Contents) {
            true
        } else if depth > 64 {
            false
        } else {
            let kids: Vec<NodeId> = self.doc.children(n).to_vec();
            kids.into_iter().any(|c| self.doc.element(c).is_some() && self.blockish(c, depth + 1))
        };
        self.blockish[n] = Some(r);
        r
    }

    fn inline_level(&mut self, n: NodeId, s: &Style) -> bool {
        if s.is_float() || replaced_kind(self.doc, n).is_some() {
            return true;
        }
        s.display.is_inline_level() && !self.blockish(n, 0)
    }

    fn text_style(&self, s: &Style, ctx: Ctx, node: Option<NodeId>) -> TextStyle {
        TextStyle {
            face: s.face(),
            color: s.color,
            deco: ctx.deco,
            deco_color: ctx.deco_color,
            letter_spacing: s.letter_spacing,
            shift: ctx.shift,
            bg: ctx.bg,
            line_h: s.line_px(),
            ws: s.white_space,
            link: ctx.link,
            hidden: s.hidden,
            node,
            elem: ctx.elem,
        }
    }

    /// Context for the content of an element with style `s`.
    fn child_ctx(&mut self, n: NodeId, s: &Style, ctx: Ctx, inline: bool) -> Ctx {
        let mut c = ctx;
        if s.decoration != 0 {
            c.deco |= s.decoration;
            c.deco_color = Some(s.decoration_color.unwrap_or(s.color));
        }
        if self.doc.is(n, "a") || self.doc.is(n, "area") {
            if let Some(href) = self.doc.element(n).and_then(|e| e.attr("href")) {
                if self.links.len() < 50_000 {
                    self.links.push(href.trim().to_string());
                    c.link = Some(self.links.len() as u32 - 1);
                }
            }
        }
        c.elem = if inline { Some(n) } else { None };
        if inline {
            match s.valign {
                VAlign::Sub => c.shift -= s.font_size * 0.25,
                VAlign::Super => c.shift += s.font_size * 0.4,
                VAlign::Length(l) => c.shift += l,
                _ => {}
            }
            if !s.background.is_transparent() {
                c.bg = Some(s.background);
            }
        } else {
            c.shift = 0.0;
            c.bg = None;
        }
        c
    }

    fn push_text(&self, text: &str, s: &Style, ctx: Ctx, out: &mut Vec<Inl>) {
        if text.is_empty() {
            return;
        }
        let mut t = crate::font::normalize(text);
        match s.transform {
            TextTransform::Upper => t = t.to_uppercase(),
            TextTransform::Lower => t = t.to_lowercase(),
            TextTransform::Capitalize => {
                let mut o = String::with_capacity(t.len());
                let mut start = true;
                for ch in t.chars() {
                    if start && ch.is_alphabetic() {
                        o.extend(ch.to_uppercase());
                    } else {
                        o.push(ch);
                    }
                    start = ch.is_whitespace() || ch == '-';
                }
                t = o;
            }
            TextTransform::None => {}
        }
        let ts = Rc::new(self.text_style(s, ctx, None));
        // Merge with a preceding run of the same style.
        if let Some(Inl::Text(prev, pt)) = out.last_mut() {
            if **prev == *ts {
                pt.push_str(&t);
                return;
            }
        }
        out.push(Inl::Text(ts, t));
    }

    fn pseudo(&mut self, n: NodeId, after: bool, ctx: Ctx, out: &mut Vec<Inl>) {
        let map = if after { &self.styles.after } else { &self.styles.before };
        let Some(ps) = map.get(&n).cloned() else { return };
        if let Some(text) = ps.content.clone() {
            let c = self.child_ctx(n, &ps, ctx, true);
            if matches!(ps.position, crate::css::style::Position::Absolute | crate::css::style::Position::Fixed) && ps.display != Display::None {
                // An out-of-flow generated box: its own block, placed by the line.
                let mut b = LBox::new(ps.clone(), None, Kind::Block);
                self.push_text(&text, &ps, atomic_ctx(c), &mut b.inlines);
                out.push(Inl::Atomic(Box::new(b), None));
                return;
            }
            self.push_text(&text, &ps, c, out);
        }
    }

    /// Inline content of element `n` (inline-level) into `out`.
    fn collect_inline(&mut self, n: NodeId, s: Rc<Style>, ctx: Ctx, out: &mut Vec<Inl>, depth: usize) {
        self.boxes += 1;
        if self.boxes > MAX_BOXES {
            return;
        }
        if self.doc.is(n, "br") {
            out.push(Inl::Br(Rc::new(self.text_style(&s, ctx, None))));
            return;
        }
        if self.doc.is(n, "wbr") {
            return;
        }
        if self.is_atomic(n, &s) || depth > 150 {
            let link = self.child_ctx(n, &s, ctx, true).link;
            let b = self.build_box(n, s, atomic_ctx(ctx));
            out.push(Inl::Atomic(Box::new(b), link));
            return;
        }
        let c = self.child_ctx(n, &s, ctx, true);
        let ext = |side: usize| s.margin[side].resolve(Some(0.0)).unwrap_or(0.0) + s.border(side) + s.padding[side].resolve(Some(0.0)).unwrap_or(0.0);
        let left = ext(3);
        let right = ext(1);
        let has_id = self.doc.element(n).is_some_and(|e| e.id().is_some() || (e.name == "a" && e.attr("name").is_some()));
        let boxed = left != 0.0 || right != 0.0 || has_id;
        if boxed {
            out.push(Inl::Open(Rc::new(self.text_style(&s, c, Some(n))), left, has_id.then_some(n)));
        }
        self.pseudo(n, false, c, out);
        let mut kids = Vec::new();
        self.flat_children(n, &mut kids, 0);
        for k in kids {
            match &self.doc.nodes[k].data {
                NodeData::Text(t) => {
                    let t = t.clone();
                    self.push_text(&t, &s, c, out);
                }
                NodeData::Element(_) => {
                    let Some(ks) = self.shown(k) else { continue };
                    if self.inline_level(k, &ks) {
                        self.collect_inline(k, ks, c, out, depth + 1);
                    } else {
                        // A block inside an inline that wasn't recognised
                        // as blockish (too deep): an atomic box.
                        let b = self.build_box(k, ks, c);
                        out.push(Inl::Atomic(Box::new(b), c.link));
                    }
                }
                _ => {}
            }
        }
        self.pseudo(n, true, c, out);
        if boxed {
            out.push(Inl::Close(right));
        }
    }

    /// The box for element `n` (block-level, or an atomic inline).
    fn build_box(&mut self, n: NodeId, s: Rc<Style>, ctx: Ctx) -> LBox {
        self.boxes += 1;
        if let Some(r) = replaced_kind(self.doc, n) {
            return LBox::new(s, Some(n), Kind::Replaced(r));
        }
        let c = self.child_ctx(n, &s, ctx, false);
        match s.display {
            Display::Table | Display::InlineTable => {
                let t = self.build_table(n, c);
                LBox::new(s, Some(n), Kind::Table(t))
            }
            Display::Flex | Display::InlineFlex => {
                let mut b = LBox::new(s, Some(n), Kind::Flex);
                b.children = self.build_items(n, c);
                b
            }
            Display::Grid | Display::InlineGrid => {
                let mut b = LBox::new(s, Some(n), Kind::Grid);
                b.children = self.build_items(n, c);
                b
            }
            _ => {
                let mut b = LBox::new(s.clone(), Some(n), Kind::Block);
                if s.display == Display::ListItem && s.list_style != ListStyle::None {
                    b.marker = Some(Marker::Bullet(s.list_style));
                }
                if self.boxes <= MAX_BOXES {
                    self.fill_container(n, &mut b, c);
                }
                b
            }
        }
    }

    fn anon(&self, parent: &Style, inl: Vec<Inl>) -> LBox {
        let mut st = Style::inherit(parent);
        st.display = Display::Block;
        let mut b = LBox::new(Rc::new(st), None, Kind::Block);
        b.inlines = inl;
        b
    }

    fn only_space(inl: &[Inl]) -> bool {
        inl.iter().all(|i| match i {
            Inl::Text(ts, t) => ts.ws.collapses_spaces() && !ts.ws.keeps_newlines() && t.chars().all(|c| matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0C')),
            _ => false,
        })
    }

    /// Fills a block container: block children, or inline content.
    fn fill_container(&mut self, n: NodeId, b: &mut LBox, ctx: Ctx) {
        let s = b.style.clone();
        let mut kids = Vec::new();
        self.flat_children(n, &mut kids, 0);
        let mut blocks: Vec<LBox> = Vec::new();
        let mut cur: Vec<Inl> = Vec::new();
        let mut any_block = false;
        self.pseudo(n, false, ctx_inline_start(ctx), &mut cur);
        // List numbering.
        let is_ol = self.doc.is(n, "ol");
        let reversed = is_ol && self.doc.element(n).is_some_and(|e| e.attr("reversed").is_some());
        let mut counter: i64 = if is_ol {
            match self.doc.element(n).and_then(|e| e.attr("start")).and_then(|v| v.trim().parse::<i64>().ok()) {
                Some(v) => v,
                None if reversed => kids.iter().filter(|&&k| self.style(k).is_some_and(|s| s.display == Display::ListItem)).count() as i64,
                None => 1,
            }
        } else {
            1
        };
        for k in kids {
            if self.boxes > MAX_BOXES {
                break;
            }
            match &self.doc.nodes[k].data {
                NodeData::Text(t) => {
                    let t = t.clone();
                    self.push_text(&t, &s, ctx_inline_start(ctx), &mut cur);
                }
                NodeData::Element(_) => {
                    let Some(ks) = self.shown(k) else { continue };
                    if self.inline_level(k, &ks) {
                        self.collect_inline(k, ks, ctx_inline_start(ctx), &mut cur, 0);
                    } else {
                        any_block = true;
                        if !Self::only_space(&cur) {
                            let a = self.anon(&s, std::mem::take(&mut cur));
                            blocks.push(a);
                        } else {
                            cur.clear();
                        }
                        let mut cb = self.build_box(k, ks.clone(), ctx);
                        if ks.display == Display::ListItem {
                            if let Some(v) = self.doc.element(k).and_then(|e| e.attr("value")).and_then(|v| v.trim().parse::<i64>().ok()) {
                                counter = v;
                            }
                            if let Some(Marker::Bullet(ls)) = cb.marker {
                                if let Some(text) = ordinal(ls, counter) {
                                    cb.marker = Some(Marker::Text(text));
                                }
                            }
                            counter += if reversed { -1 } else { 1 };
                        }
                        blocks.push(cb);
                    }
                }
                _ => {}
            }
        }
        self.pseudo(n, true, ctx_inline_start(ctx), &mut cur);
        if !any_block {
            b.inlines = cur;
        } else {
            if !Self::only_space(&cur) {
                let a = self.anon(&s, cur);
                blocks.push(a);
            }
            b.children = blocks;
        }
    }

    /// Flex and grid items: every child element is a block-level item;
    /// runs of text become anonymous items.
    fn build_items(&mut self, n: NodeId, ctx: Ctx) -> Vec<LBox> {
        let s = self.style(n).unwrap_or_else(|| Rc::new(Style::initial()));
        let mut kids = Vec::new();
        self.flat_children(n, &mut kids, 0);
        let mut items: Vec<LBox> = Vec::new();
        let mut cur: Vec<Inl> = Vec::new();
        self.pseudo(n, false, ctx_inline_start(ctx), &mut cur);
        for k in kids {
            if self.boxes > MAX_BOXES {
                break;
            }
            match &self.doc.nodes[k].data {
                NodeData::Text(t) => {
                    let t = t.clone();
                    self.push_text(&t, &s, ctx_inline_start(ctx), &mut cur);
                }
                NodeData::Element(_) => {
                    let Some(ks) = self.shown(k) else { continue };
                    if !Self::only_space(&cur) {
                        let a = self.anon(&s, std::mem::take(&mut cur));
                        items.push(a);
                    } else {
                        cur.clear();
                    }
                    // Blockify.
                    let mut st = (*ks).clone();
                    st.display = match st.display {
                        Display::Inline | Display::InlineBlock => Display::Block,
                        Display::InlineFlex => Display::Flex,
                        Display::InlineGrid => Display::Grid,
                        Display::InlineTable => Display::Table,
                        d => d,
                    };
                    st.float = Float::None;
                    let st = Rc::new(st);
                    if replaced_kind(self.doc, k).is_none() && self.inline_level(k, &ks) && ks.display == Display::Inline {
                        // An inline element as an item: a block holding its inline content.
                        let mut b = LBox::new(st.clone(), Some(k), Kind::Block);
                        let mut inl = Vec::new();
                        self.collect_inline(k, ks, ctx_inline_start(ctx), &mut inl, 0);
                        b.inlines = inl;
                        items.push(b);
                    } else {
                        items.push(self.build_box(k, st, ctx));
                    }
                }
                _ => {}
            }
        }
        self.pseudo(n, true, ctx_inline_start(ctx), &mut cur);
        if !Self::only_space(&cur) {
            let a = self.anon(&s, cur);
            items.push(a);
        }
        items.sort_by_key(|b| b.style.order);
        items
    }

    fn build_table(&mut self, n: NodeId, ctx: Ctx) -> TableData {
        let mut t = TableData { caption: None, rows: Vec::new() };
        let mut head: Vec<Row> = Vec::new();
        let mut body: Vec<Row> = Vec::new();
        let mut foot: Vec<Row> = Vec::new();
        let table_style = self.style(n).unwrap_or_else(|| Rc::new(Style::initial()));
        let mut kids = Vec::new();
        self.flat_children(n, &mut kids, 0);
        let mut loose: Vec<NodeId> = Vec::new();
        for k in kids {
            if self.boxes > MAX_BOXES || head.len() + body.len() + foot.len() > 5000 {
                break;
            }
            let Some(ks) = self.shown(k) else {
                if let NodeData::Text(txt) = &self.doc.nodes[k].data {
                    if !txt.trim().is_empty() {
                        loose.push(k);
                    }
                }
                continue;
            };
            match ks.display {
                Display::TableCaption => {
                    if t.caption.is_none() {
                        t.caption = Some(Box::new(self.build_box(k, ks, ctx)));
                    }
                }
                Display::TableHeaderGroup | Display::TableRowGroup | Display::TableFooterGroup => {
                    let mut gk = Vec::new();
                    self.flat_children(k, &mut gk, 0);
                    let rows = self.build_rows(&gk, &ks, ctx);
                    match ks.display {
                        Display::TableHeaderGroup => head.extend(rows),
                        Display::TableFooterGroup => foot.extend(rows),
                        _ => body.extend(rows),
                    }
                }
                Display::TableColumn | Display::TableColumnGroup => {}
                _ => loose.push(k),
            }
        }
        if !loose.is_empty() {
            let rows = self.build_rows(&loose, &table_style, ctx);
            body.extend(rows);
        }
        t.rows = head;
        t.rows.extend(body);
        t.rows.extend(foot);
        t
    }

    /// Rows from a row group's children: <tr>s, with loose cells (and any
    /// other content) gathered into anonymous rows.
    fn build_rows(&mut self, kids: &[NodeId], group: &Rc<Style>, ctx: Ctx) -> Vec<Row> {
        let mut rows = Vec::new();
        let mut loose: Vec<NodeId> = Vec::new();
        for &k in kids {
            if rows.len() > 5000 {
                break;
            }
            let Some(ks) = self.shown(k) else {
                if let NodeData::Text(txt) = &self.doc.nodes[k].data {
                    if !txt.trim().is_empty() {
                        loose.push(k);
                    }
                }
                continue;
            };
            if ks.display == Display::TableRow {
                if !loose.is_empty() {
                    let r = self.build_row(&std::mem::take(&mut loose), group, ctx);
                    rows.push(r);
                }
                let mut rk = Vec::new();
                self.flat_children(k, &mut rk, 0);
                let r = self.build_row(&rk, &ks, ctx);
                rows.push(r);
            } else {
                loose.push(k);
            }
        }
        if !loose.is_empty() {
            let r = self.build_row(&loose, group, ctx);
            rows.push(r);
        }
        rows
    }

    fn build_row(&mut self, kids: &[NodeId], row_style: &Rc<Style>, ctx: Ctx) -> Row {
        let mut row = Row { style: row_style.clone(), cells: Vec::new() };
        let mut loose: Vec<Inl> = Vec::new();
        let mut loose_blocks: Vec<LBox> = Vec::new();
        for &k in kids {
            if row.cells.len() >= 256 || self.boxes > MAX_BOXES {
                break;
            }
            match &self.doc.nodes[k].data {
                NodeData::Text(t) => {
                    if !t.trim().is_empty() {
                        let t = t.clone();
                        self.push_text(&t, row_style, ctx_inline_start(ctx), &mut loose);
                    }
                }
                NodeData::Element(e) => {
                    let Some(ks) = self.shown(k) else { continue };
                    if ks.display == Display::TableCell {
                        let colspan = e.attr("colspan").and_then(|v| v.trim().parse::<usize>().ok()).unwrap_or(1).clamp(1, 64);
                        let rowspan = e.attr("rowspan").and_then(|v| v.trim().parse::<usize>().ok()).unwrap_or(1).clamp(1, 256);
                        let mut st = (*ks).clone();
                        st.display = Display::Block;
                        let b = self.build_box(k, Rc::new(st), ctx);
                        row.cells.push(Cell_ { b, colspan, rowspan });
                    } else if self.inline_level(k, &ks) {
                        self.collect_inline(k, ks, ctx_inline_start(ctx), &mut loose, 0);
                    } else {
                        loose_blocks.push(self.build_box(k, ks, ctx));
                    }
                }
                _ => {}
            }
        }
        if !loose.is_empty() || !loose_blocks.is_empty() {
            let mut st = Style::inherit(row_style);
            st.display = Display::Block;
            let mut b = LBox::new(Rc::new(st), None, Kind::Block);
            if loose_blocks.is_empty() {
                b.inlines = loose;
            } else {
                if !loose.is_empty() {
                    loose_blocks.insert(0, self.anon(row_style, loose));
                }
                b.children = loose_blocks;
            }
            row.cells.push(Cell_ { b, colspan: 1, rowspan: 1 });
        }
        row
    }
}

/// Inline content of a block starts with the block's decorations and link.
fn ctx_inline_start(ctx: Ctx) -> Ctx {
    Ctx { shift: 0.0, bg: None, ..ctx }
}

fn atomic_ctx(ctx: Ctx) -> Ctx {
    // Atomic inlines don't inherit text decoration or backgrounds; links do.
    Ctx { link: ctx.link, ..NO_CTX }
}

/// A list marker's text, e.g. "3." or "iv.".
pub fn ordinal(ls: ListStyle, n: i64) -> Option<String> {
    let n = n.clamp(-999_999, 999_999);
    Some(match ls {
        ListStyle::Decimal => format!("{n}."),
        ListStyle::DecimalLeadingZero => format!("{n:02}."),
        ListStyle::LowerAlpha | ListStyle::UpperAlpha => {
            if n < 1 {
                return Some(format!("{n}."));
            }
            let mut v = n;
            let mut s = Vec::new();
            while v > 0 {
                v -= 1;
                s.push((b'a' + (v % 26) as u8) as char);
                v /= 26;
            }
            let t: String = s.into_iter().rev().collect();
            format!("{}.", if ls == ListStyle::UpperAlpha { t.to_uppercase() } else { t })
        }
        ListStyle::LowerRoman | ListStyle::UpperRoman => {
            if !(1..4000).contains(&n) {
                return Some(format!("{n}."));
            }
            let table = [(1000, "m"), (900, "cm"), (500, "d"), (400, "cd"), (100, "c"), (90, "xc"), (50, "l"), (40, "xl"), (10, "x"), (9, "ix"), (5, "v"), (4, "iv"), (1, "i")];
            let mut v = n;
            let mut s = String::new();
            for (k, r) in table {
                while v >= k {
                    s += r;
                    v -= k;
                }
            }
            format!("{}.", if ls == ListStyle::UpperRoman { s.to_uppercase() } else { s })
        }
        ListStyle::LowerGreek => {
            if n < 1 {
                return Some(format!("{n}."));
            }
            let c = char::from_u32(0x3B1 + ((n - 1) % 24) as u32).unwrap_or('?');
            format!("{c}.")
        }
        _ => return None,
    })
}
