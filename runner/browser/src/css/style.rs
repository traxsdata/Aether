//! style.rs - the cascade: which declarations apply to each element, in what
//! order, and the computed values layout uses.
//!
//! Origins: the user-agent sheet (ua.css), HTML presentational attributes
//! (bgcolor, width, align ...), author sheets, then style="" attributes, with
//! !important reversing the order of origins. Within an origin, specificity
//! then source order. Custom properties (--x) and var() are supported, with
//! cycle and size limits.

use std::collections::HashMap;
use std::rc::Rc;

use super::matching::matches;
use super::parser::{parse_inline, Declaration, MediaEnv, PseudoElement, Selector, Stylesheet};
use super::tokenizer::{component_values, tokenize, Cv, Tok};
use super::values::{parse_color, parse_len, unit_px, Color, Len, Units};
use crate::dom::{Document, NodeData, NodeId};

/// Properties `@supports (prop: value)` reports as supported.
pub const SUPPORTED: &[&str] = &[
    "color", "background", "background-color", "margin", "padding", "border", "width", "max-width", "min-width", "height",
    "font-size", "font-weight", "font-style", "text-align", "text-decoration", "line-height", "white-space", "list-style",
    "list-style-type", "gap", "flex", "flex-direction", "flex-wrap", "justify-content", "align-items", "border-radius",
    "box-sizing", "grid-template-columns",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Display {
    None,
    Block,
    Inline,
    InlineBlock,
    ListItem,
    Table,
    InlineTable,
    TableRowGroup,
    TableHeaderGroup,
    TableFooterGroup,
    TableRow,
    TableCell,
    TableCaption,
    TableColumn,
    TableColumnGroup,
    Flex,
    InlineFlex,
    Grid,
    InlineGrid,
    Contents,
}

impl Display {
    pub fn is_inline_level(self) -> bool {
        matches!(self, Display::Inline | Display::InlineBlock | Display::InlineTable | Display::InlineFlex | Display::InlineGrid)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Position {
    Static,
    Relative,
    Absolute,
    Fixed,
    Sticky,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Float {
    None,
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextAlign {
    Left,
    Right,
    Center,
    Justify,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WhiteSpace {
    Normal,
    Pre,
    NoWrap,
    PreWrap,
    PreLine,
}

impl WhiteSpace {
    pub fn collapses_spaces(self) -> bool {
        matches!(self, WhiteSpace::Normal | WhiteSpace::NoWrap | WhiteSpace::PreLine)
    }
    pub fn keeps_newlines(self) -> bool {
        matches!(self, WhiteSpace::Pre | WhiteSpace::PreWrap | WhiteSpace::PreLine)
    }
    pub fn wraps(self) -> bool {
        matches!(self, WhiteSpace::Normal | WhiteSpace::PreWrap | WhiteSpace::PreLine)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextTransform {
    None,
    Upper,
    Lower,
    Capitalize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListStyle {
    None,
    Disc,
    Circle,
    Square,
    Decimal,
    DecimalLeadingZero,
    LowerAlpha,
    UpperAlpha,
    LowerRoman,
    UpperRoman,
    LowerGreek,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum VAlign {
    Baseline,
    Sub,
    Super,
    Top,
    Middle,
    Bottom,
    TextTop,
    TextBottom,
    Length(f32),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LineHeight {
    Normal,
    Number(f32),
    Px(f32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BorderStyle {
    None,
    Solid,
    Dashed,
    Dotted,
    Double,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Justify {
    Start,
    End,
    Center,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Stretch,
    Start,
    End,
    Center,
    Baseline,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Track {
    Fr(f32),
    Len(Len),
    Auto,
    /// repeat(auto-fill / auto-fit, minmax(min, 1fr))
    AutoFill(f32),
}

pub const DECO_UNDERLINE: u8 = 1;
pub const DECO_OVERLINE: u8 = 2;
pub const DECO_LINE_THROUGH: u8 = 4;

#[derive(Clone, Debug)]
pub struct Style {
    pub display: Display,
    pub position: Position,
    pub float: Float,
    pub hidden: bool, // visibility: hidden or opacity 0
    pub opacity: f32,
    pub color: Color,
    pub background: Color,
    /// A linear gradient background: (from, to, vertical).
    pub gradient: Option<(Color, Color, bool)>,
    pub has_bg_image: bool,
    pub font_size: f32,
    pub weight: u16,
    pub italic: bool,
    pub mono: bool,
    pub line_height: LineHeight,
    pub text_align: TextAlign,
    pub decoration: u8,
    pub decoration_color: Option<Color>,
    pub transform: TextTransform,
    pub indent: Len,
    pub white_space: WhiteSpace,
    pub letter_spacing: f32,
    pub list_style: ListStyle,
    pub list_inside: bool,
    pub valign: VAlign,
    pub margin: [Len; 4],
    pub padding: [Len; 4],
    pub border_width: [f32; 4],
    pub border_style: [BorderStyle; 4],
    /// None: currentColor.
    pub border_color: [Option<Color>; 4],
    pub radius: Len,
    pub width: Len,
    pub height: Len,
    pub min_width: Len,
    pub max_width: Len,
    pub min_height: Len,
    pub max_height: Len,
    pub border_box: bool,
    pub overflow_hidden: bool,
    pub inset: [Len; 4],
    /// clip / clip-path that hides everything (screen-reader-only text).
    pub clipped: bool,
    pub flex_column: bool,
    pub flex_wrap: bool,
    pub flex_reverse: bool,
    pub justify: Justify,
    pub align_items: Align,
    pub gap: (Len, Len), // (row, column)
    pub grow: f32,
    pub shrink: f32,
    pub basis: Len,
    pub order: i32,
    pub grid_columns: Vec<Track>,
    pub grid_span: u16,
    pub collapse: bool,
    pub spacing: f32,
    pub cell_valign: VAlign,
    pub content: Option<String>,
    pub vars: Rc<HashMap<String, Vec<Cv>>>,
}

impl Style {
    /// The initial style of the root (CSS initial values, 16px text).
    pub fn initial() -> Style {
        Style {
            display: Display::Inline,
            position: Position::Static,
            float: Float::None,
            hidden: false,
            opacity: 1.0,
            color: Color::BLACK,
            background: Color::TRANSPARENT,
            gradient: None,
            has_bg_image: false,
            font_size: 16.0,
            weight: 400,
            italic: false,
            mono: false,
            line_height: LineHeight::Normal,
            text_align: TextAlign::Left,
            decoration: 0,
            decoration_color: None,
            transform: TextTransform::None,
            indent: Len::ZERO,
            white_space: WhiteSpace::Normal,
            letter_spacing: 0.0,
            list_style: ListStyle::Disc,
            list_inside: false,
            valign: VAlign::Baseline,
            margin: [Len::ZERO; 4],
            padding: [Len::ZERO; 4],
            border_width: [3.0; 4],
            border_style: [BorderStyle::None; 4],
            border_color: [None; 4],
            radius: Len::ZERO,
            width: Len::Auto,
            height: Len::Auto,
            min_width: Len::Auto,
            max_width: Len::None,
            min_height: Len::Auto,
            max_height: Len::None,
            border_box: false,
            overflow_hidden: false,
            inset: [Len::Auto; 4],
            clipped: false,
            flex_column: false,
            flex_wrap: false,
            flex_reverse: false,
            justify: Justify::Start,
            align_items: Align::Stretch,
            gap: (Len::ZERO, Len::ZERO),
            grow: 0.0,
            shrink: 1.0,
            basis: Len::Auto,
            order: 0,
            grid_columns: Vec::new(),
            grid_span: 1,
            collapse: false,
            spacing: 0.0,
            cell_valign: VAlign::Middle,
            content: None,
            vars: Rc::new(HashMap::new()),
        }
    }

    /// A child's starting point: inherited properties from the parent,
    /// everything else initial.
    pub fn inherit(parent: &Style) -> Style {
        Style {
            color: parent.color,
            font_size: parent.font_size,
            weight: parent.weight,
            italic: parent.italic,
            mono: parent.mono,
            line_height: parent.line_height,
            text_align: parent.text_align,
            transform: parent.transform,
            indent: parent.indent,
            white_space: parent.white_space,
            letter_spacing: parent.letter_spacing,
            list_style: parent.list_style,
            list_inside: parent.list_inside,
            hidden: parent.hidden,
            collapse: parent.collapse,
            spacing: parent.spacing,
            cell_valign: parent.cell_valign,
            vars: parent.vars.clone(),
            ..Style::initial()
        }
    }

    pub fn bold(&self) -> bool {
        self.weight >= 600
    }

    pub fn face(&self) -> crate::font::Face {
        crate::font::Face::new(self.font_size, self.bold(), self.italic, self.mono)
    }

    /// The used line height in px.
    pub fn line_px(&self) -> f32 {
        match self.line_height {
            LineHeight::Normal => (self.font_size * 1.3).round(),
            LineHeight::Number(n) => self.font_size * n,
            LineHeight::Px(p) => p,
        }
    }

    pub fn border_color(&self, side: usize) -> Color {
        self.border_color[side].unwrap_or(self.color)
    }

    /// Used border width (0 when the style is none).
    pub fn border(&self, side: usize) -> f32 {
        if self.border_style[side] == BorderStyle::None {
            0.0
        } else {
            self.border_width[side]
        }
    }

    pub fn is_float(&self) -> bool {
        self.float != Float::None && !matches!(self.position, Position::Absolute | Position::Fixed)
    }

    /// Absolutely positioned and placed far off-screen, or clipped away:
    /// the usual ways pages hide text that is only for screen readers.
    pub fn is_offscreen(&self) -> bool {
        if !matches!(self.position, Position::Absolute | Position::Fixed) {
            return false;
        }
        if self.clipped {
            return true;
        }
        let far = |l: Len| matches!(l, Len::Calc(px, _) if px <= -999.0);
        if far(self.inset[3]) || far(self.inset[0]) || far(self.inset[1]) {
            return true;
        }
        let tiny = |l: Len| matches!(l, Len::Calc(px, pct) if pct == 0.0 && px <= 1.0);
        self.overflow_hidden && (tiny(self.width) || tiny(self.height))
    }
}

// ---- the user-agent stylesheet ------------------------------------------

pub const UA_CSS: &str = include_str!("ua.css");

fn ua_sheet() -> &'static Stylesheet {
    use std::sync::OnceLock;
    static UA: OnceLock<Stylesheet> = OnceLock::new();
    UA.get_or_init(|| Stylesheet::parse(UA_CSS, &MediaEnv::default()))
}

// ---- rule index ---------------------------------------------------------

struct Entry<'a> {
    sel: &'a Selector,
    decls: &'a [Declaration],
    level: u8,
    order: u32,
}

#[derive(Default)]
struct Index<'a> {
    by_id: HashMap<&'a str, Vec<Entry<'a>>>,
    by_class: HashMap<&'a str, Vec<Entry<'a>>>,
    by_tag: HashMap<&'a str, Vec<Entry<'a>>>,
    rest: Vec<Entry<'a>>,
    before: Vec<Entry<'a>>,
    after: Vec<Entry<'a>>,
}

impl<'a> Index<'a> {
    fn add(&mut self, sheet: &'a Stylesheet, level: u8, order: &mut u32) {
        for rule in &sheet.rules {
            for sel in &rule.selectors {
                *order += 1;
                let e = Entry { sel, decls: &rule.decls, level, order: *order };
                match sel.pseudo_element {
                    Some(PseudoElement::Before) => {
                        self.before.push(e);
                        continue;
                    }
                    Some(PseudoElement::After) => {
                        self.after.push(e);
                        continue;
                    }
                    Some(_) => continue,
                    None => {}
                }
                let last = sel.compounds.last().unwrap();
                if let Some(id) = &last.id {
                    self.by_id.entry(id.as_str()).or_default().push(e);
                } else if let Some(c) = last.classes.first() {
                    self.by_class.entry(c.as_str()).or_default().push(e);
                } else if let Some(t) = &last.tag {
                    self.by_tag.entry(t.as_str()).or_default().push(e);
                } else {
                    self.rest.push(e);
                }
            }
        }
    }

    fn candidates<'b>(&'b self, doc: &Document, el: NodeId, out: &mut Vec<&'b Entry<'a>>) {
        let Some(e) = doc.element(el) else { return };
        out.extend(self.rest.iter());
        if let Some(v) = self.by_tag.get(e.name.as_str()) {
            out.extend(v.iter());
        }
        if let Some(id) = e.id() {
            if let Some(v) = self.by_id.get(id) {
                out.extend(v.iter());
            }
        }
        if let Some(cl) = e.attr("class") {
            let mut seen: Vec<&str> = Vec::new();
            for c in cl.split_ascii_whitespace().take(32) {
                if seen.contains(&c) {
                    continue;
                }
                seen.push(c);
                if let Some(v) = self.by_class.get(c) {
                    out.extend(v.iter());
                }
            }
        }
    }
}

/// Levels, lowest first: UA, presentational hints + author, inline, then the
/// important ones in reverse.
const L_UA: u8 = 0;
const L_AUTHOR: u8 = 1;
const L_INLINE: u8 = 2;

fn sort_key(level: u8, important: bool) -> u8 {
    if !important {
        level
    } else {
        // author !important < inline !important < UA !important
        match level {
            L_UA => 6,
            L_INLINE => 5,
            _ => 4,
        }
    }
}

/// Computed styles for every element of `doc` (None for non-elements and
/// for elements under a display:none ancestor are still computed; layout
/// decides). Also ::before / ::after content, keyed by element.
pub struct Styles {
    pub of: Vec<Option<Rc<Style>>>,
    pub before: HashMap<NodeId, Rc<Style>>,
    pub after: HashMap<NodeId, Rc<Style>>,
}

impl Styles {
    pub fn get(&self, n: NodeId) -> Option<&Rc<Style>> {
        self.of.get(n).and_then(|s| s.as_ref())
    }
}

pub fn compute_styles(doc: &Document, author: &[Stylesheet], env: &MediaEnv) -> Styles {
    let mut index = Index::default();
    let mut order = 0;
    index.add(ua_sheet(), L_UA, &mut order);
    for s in author {
        index.add(s, L_AUTHOR, &mut order);
    }
    let mut of: Vec<Option<Rc<Style>>> = vec![None; doc.nodes.len()];
    let mut before = HashMap::new();
    let mut after = HashMap::new();
    let root_style = Rc::new(Style::initial());
    let mut rem = 16.0;
    // Tree order, iteratively: each element's parent is styled first.
    let mut stack: Vec<NodeId> = doc.children(Document::ROOT).iter().rev().copied().collect();
    let mut cands = Vec::new();
    while let Some(n) = stack.pop() {
        for &c in doc.children(n).iter().rev() {
            stack.push(c);
        }
        let Some(el) = doc.element(n) else { continue };
        let parent = doc.parent(n).and_then(|p| of[p].clone()).unwrap_or_else(|| root_style.clone());
        cands.clear();
        index.candidates(doc, n, &mut cands);
        let mut decls: Vec<(u8, (u16, u16, u16), u32, usize, &Declaration)> = Vec::new();
        for e in cands.iter() {
            if matches(doc, n, e.sel) {
                for (i, d) in e.decls.iter().enumerate() {
                    decls.push((sort_key(e.level, d.important), e.sel.specificity, e.order, i, d));
                }
            }
        }
        let hints = presentational_hints(doc, n);
        for (i, d) in hints.iter().enumerate() {
            decls.push((sort_key(L_AUTHOR, false), (0, 0, 0), 0, i, d));
        }
        let inline = el.attr("style").map(parse_inline).unwrap_or_default();
        for (i, d) in inline.iter().enumerate() {
            decls.push((sort_key(L_INLINE, d.important), (0, 0, 0), u32::MAX, i, d));
        }
        decls.sort_by(|a, b| (a.0, a.1, a.2, a.3).cmp(&(b.0, b.1, b.2, b.3)));
        let ordered: Vec<&Declaration> = decls.iter().map(|d| d.4).collect();
        let is_root = doc.parent(n) == Some(Document::ROOT);
        let units_root = if is_root { None } else { Some(rem) };
        let mut style = cascade(&parent, &ordered, env, units_root);
        if is_root {
            rem = style.font_size;
        }
        fixups(&mut style, doc, n, &parent);
        // ::before and ::after with string content.
        for (list, map) in [(&index.before, &mut before), (&index.after, &mut after)] {
            let mut pd: Vec<(u8, (u16, u16, u16), u32, usize, &Declaration)> = Vec::new();
            for e in list.iter() {
                if matches(doc, n, e.sel) {
                    for (i, d) in e.decls.iter().enumerate() {
                        pd.push((sort_key(e.level, d.important), e.sel.specificity, e.order, i, d));
                    }
                }
            }
            if pd.is_empty() {
                continue;
            }
            pd.sort_by(|a, b| (a.0, a.1, a.2, a.3).cmp(&(b.0, b.1, b.2, b.3)));
            let ordered: Vec<&Declaration> = pd.iter().map(|d| d.4).collect();
            let ps = cascade(&style, &ordered, env, Some(rem));
            if ps.content.as_deref().is_some_and(|c| !c.is_empty()) && ps.display != Display::None {
                map.insert(n, Rc::new(ps));
            }
        }
        of[n] = Some(Rc::new(style));
    }
    Styles { of, before, after }
}

/// Element-specific adjustments after the cascade.
fn fixups(s: &mut Style, doc: &Document, n: NodeId, parent: &Style) {
    let name = doc.name(n);
    // Floats and absolutely positioned boxes are block-level ("blockified").
    if s.float != Float::None || matches!(s.position, Position::Absolute | Position::Fixed) {
        s.display = match s.display {
            Display::Inline | Display::InlineBlock => Display::Block,
            Display::InlineFlex => Display::Flex,
            Display::InlineGrid => Display::Grid,
            Display::InlineTable => Display::Table,
            d => d,
        };
    }
    // A table cell's vertical-align comes from cell_valign.
    if name == "td" || name == "th" {
        if s.valign != VAlign::Baseline {
            s.cell_valign = s.valign;
        }
    }
    if s.opacity <= 0.01 {
        s.hidden = true;
    }
    let _ = parent;
}

/// Applies `decls` (lowest priority first) on top of the inherited style.
pub fn cascade(parent: &Style, decls: &[&Declaration], env: &MediaEnv, rem: Option<f32>) -> Style {
    let mut s = Style::inherit(parent);
    // 1. Custom properties.
    if decls.iter().any(|d| d.name.starts_with("--")) {
        let mut vars = (*parent.vars).clone();
        for d in decls.iter().filter(|d| d.name.starts_with("--")) {
            if vars.len() < 1000 && d.value.len() < 2000 {
                vars.insert(d.name.clone(), d.value.clone());
            }
        }
        s.vars = Rc::new(vars);
    }
    // 2. font-size (others' em units depend on it).
    let pu = Units { em: parent.font_size, rem: rem.unwrap_or(16.0), vw: env.width, vh: env.height };
    for d in decls.iter().filter(|d| d.name == "font-size" || d.name == "font") {
        let v = substitute(&d.value, &s.vars);
        if d.name == "font-size" {
            if let Some(fs) = font_size(&v, parent, &pu) {
                s.font_size = fs;
            } else if is_kw(&v, "inherit") {
                s.font_size = parent.font_size;
            } else if is_kw(&v, "initial") {
                s.font_size = 16.0;
            }
        } else {
            font_shorthand_size(&mut s, &v, parent, &pu);
        }
    }
    s.font_size = s.font_size.clamp(1.0, 400.0);
    let u = Units { em: s.font_size, rem: rem.unwrap_or(s.font_size), vw: env.width, vh: env.height };
    // 3. color (currentColor in other properties refers to it).
    for d in decls.iter().filter(|d| d.name == "color") {
        let v = substitute(&d.value, &s.vars);
        apply(&mut s, parent, "color", &v, &u);
    }
    // 4. Everything else, in order.
    for d in decls {
        if d.name.starts_with("--") || d.name == "font-size" || d.name == "color" {
            continue;
        }
        let v = substitute(&d.value, &s.vars);
        apply(&mut s, parent, &d.name, &v, &u);
    }
    s
}

fn is_kw(v: &[Cv], kw: &str) -> bool {
    let items: Vec<&Cv> = v.iter().filter(|c| !c.is_ws()).collect();
    items.len() == 1 && items[0].ident().is_some_and(|i| i.eq_ignore_ascii_case(kw))
}

/// Replaces var(--x, fallback) references, recursively, with limits.
pub fn substitute(v: &[Cv], vars: &HashMap<String, Vec<Cv>>) -> Vec<Cv> {
    fn has_var(v: &[Cv]) -> bool {
        v.iter().any(|c| match c {
            Cv::Func(f, a) => f == "var" || has_var(a),
            Cv::Block(_, a) => has_var(a),
            _ => false,
        })
    }
    if !has_var(v) {
        return v.to_vec();
    }
    let mut budget = 4096usize;
    let mut out = Vec::new();
    sub_into(v, vars, 0, &mut budget, &mut out);
    out
}

fn sub_into(v: &[Cv], vars: &HashMap<String, Vec<Cv>>, depth: usize, budget: &mut usize, out: &mut Vec<Cv>) -> bool {
    if depth > 16 {
        return false;
    }
    for c in v {
        if *budget == 0 {
            return false;
        }
        *budget -= 1;
        match c {
            Cv::Func(f, args) if f == "var" => {
                let mut parts = args.splitn(2, |c| matches!(c, Cv::Tok(Tok::Comma)));
                let name = parts.next().and_then(|p| p.iter().find_map(|c| c.ident())).unwrap_or("");
                let fallback = parts.next();
                match vars.get(name) {
                    Some(val) if !(val.len() == 1 && val[0].ident() == Some("initial")) => {
                        if !sub_into(val, vars, depth + 1, budget, out) {
                            return false;
                        }
                    }
                    _ => match fallback {
                        Some(fb) => {
                            if !sub_into(fb, vars, depth + 1, budget, out) {
                                return false;
                            }
                        }
                        None => return false,
                    },
                }
            }
            Cv::Func(f, args) => {
                let mut inner = Vec::new();
                if !sub_into(args, vars, depth + 1, budget, &mut inner) {
                    return false;
                }
                out.push(Cv::Func(f.clone(), inner));
            }
            Cv::Block(ch, args) => {
                let mut inner = Vec::new();
                if !sub_into(args, vars, depth + 1, budget, &mut inner) {
                    return false;
                }
                out.push(Cv::Block(*ch, inner));
            }
            c => out.push(c.clone()),
        }
    }
    true
}

fn font_size(v: &[Cv], parent: &Style, u: &Units) -> Option<f32> {
    let items: Vec<&Cv> = v.iter().filter(|c| !c.is_ws()).collect();
    if items.len() != 1 {
        return None;
    }
    let c = items[0];
    if let Some(k) = c.ident() {
        return Some(match k.to_ascii_lowercase().as_str() {
            "xx-small" => 9.0,
            "x-small" => 10.0,
            "small" => 13.0,
            "medium" => 16.0,
            "large" => 18.0,
            "x-large" => 24.0,
            "xx-large" => 32.0,
            "xxx-large" => 48.0,
            "smaller" => parent.font_size / 1.2,
            "larger" => parent.font_size * 1.2,
            _ => return None,
        });
    }
    match parse_len(c, u)? {
        Len::Calc(px, pct) => Some(px + parent.font_size * pct / 100.0),
        _ => None,
    }
}

fn font_shorthand_size(s: &mut Style, v: &[Cv], parent: &Style, u: &Units) {
    // [style] [weight] size[/line-height] family
    for c in v.iter().filter(|c| !c.is_ws()) {
        if matches!(c, Cv::Tok(Tok::Dimension(..)) | Cv::Tok(Tok::Percentage(..))) || c.ident().is_some_and(|k| k.contains("small") || k.contains("large") || k == "medium") {
            if let Some(fs) = font_size(std::slice::from_ref(c), parent, u) {
                s.font_size = fs;
                return;
            }
        }
    }
}

fn idents(v: &[Cv]) -> Vec<String> {
    v.iter().filter_map(|c| c.ident()).map(|s| s.to_ascii_lowercase()).collect()
}

fn values(v: &[Cv]) -> Vec<&Cv> {
    v.iter().filter(|c| !c.is_ws() && !matches!(c, Cv::Tok(Tok::Comma))).collect()
}

/// Expands 1-4 values to [top, right, bottom, left].
fn four<T: Copy>(vals: &[T]) -> Option<[T; 4]> {
    Some(match vals.len() {
        1 => [vals[0]; 4],
        2 => [vals[0], vals[1], vals[0], vals[1]],
        3 => [vals[0], vals[1], vals[2], vals[1]],
        4 => [vals[0], vals[1], vals[2], vals[3]],
        _ => return None,
    })
}

fn border_width_kw(c: &Cv, u: &Units) -> Option<f32> {
    match c.ident().map(|s| s.to_ascii_lowercase()).as_deref() {
        Some("thin") => Some(1.0),
        Some("medium") => Some(3.0),
        Some("thick") => Some(5.0),
        _ => match parse_len(c, u)? {
            Len::Calc(px, 0.0) => Some(px.max(0.0)),
            _ => None,
        },
    }
}

fn border_style_kw(c: &Cv) -> Option<BorderStyle> {
    Some(match c.ident()?.to_ascii_lowercase().as_str() {
        "none" | "hidden" => BorderStyle::None,
        "solid" | "inset" | "outset" | "groove" | "ridge" => BorderStyle::Solid,
        "dashed" => BorderStyle::Dashed,
        "dotted" => BorderStyle::Dotted,
        "double" => BorderStyle::Double,
        _ => return None,
    })
}

/// `border: 1px solid red` -> (width, style, color).
fn border_parts(v: &[Cv], u: &Units, current: Color) -> Option<(f32, BorderStyle, Option<Color>)> {
    let (mut w, mut st, mut col) = (3.0, BorderStyle::None, None);
    for c in values(v) {
        if let Some(s) = border_style_kw(c) {
            st = s;
        } else if let Some(x) = border_width_kw(c, u) {
            w = x;
        } else if let Some(cc) = parse_color(c, current) {
            col = Some(cc);
        } else {
            return None;
        }
    }
    Some((w, st, col))
}

fn side_index(s: &str) -> Option<usize> {
    match s {
        "top" | "block-start" => Some(0),
        "right" | "inline-end" => Some(1),
        "bottom" | "block-end" => Some(2),
        "left" | "inline-start" => Some(3),
        _ => None,
    }
}

fn list_style_kw(k: &str) -> Option<ListStyle> {
    Some(match k {
        "none" => ListStyle::None,
        "disc" => ListStyle::Disc,
        "circle" => ListStyle::Circle,
        "square" => ListStyle::Square,
        "decimal" => ListStyle::Decimal,
        "decimal-leading-zero" => ListStyle::DecimalLeadingZero,
        "lower-alpha" | "lower-latin" => ListStyle::LowerAlpha,
        "upper-alpha" | "upper-latin" => ListStyle::UpperAlpha,
        "lower-roman" => ListStyle::LowerRoman,
        "upper-roman" => ListStyle::UpperRoman,
        "lower-greek" => ListStyle::LowerGreek,
        _ => return None,
    })
}

fn gradient(c: &Cv, current: Color) -> Option<(Color, Color, bool)> {
    let Cv::Func(f, args) = c else { return None };
    if !f.ends_with("linear-gradient") {
        return None;
    }
    let parts: Vec<&[Cv]> = args.split(|c| matches!(c, Cv::Tok(Tok::Comma))).collect();
    let mut vertical = true;
    let mut colors = Vec::new();
    for (i, p) in parts.iter().enumerate() {
        let items: Vec<&Cv> = p.iter().filter(|c| !c.is_ws()).collect();
        if i == 0 {
            if items.first().and_then(|c| c.ident()) == Some("to") {
                vertical = !matches!(items.get(1).and_then(|c| c.ident()), Some("right") | Some("left"));
                continue;
            }
            if let Some(Cv::Tok(Tok::Dimension(deg, unit))) = items.first() {
                if unit == "deg" {
                    let d = deg.rem_euclid(180.0);
                    vertical = !(45.0..135.0).contains(&d);
                    continue;
                }
            }
        }
        if let Some(col) = items.first().and_then(|c| parse_color(c, current)) {
            colors.push(col);
        }
    }
    let first = *colors.first()?;
    let last = *colors.last()?;
    Some((first, last, vertical))
}

fn tracks(v: &[Cv], u: &Units, out: &mut Vec<Track>, depth: usize) -> bool {
    for c in v.iter().filter(|c| !c.is_ws()) {
        if out.len() > 64 || depth > 2 {
            return false;
        }
        match c {
            Cv::Tok(Tok::Dimension(n, unit)) if unit == "fr" => out.push(Track::Fr(*n)),
            Cv::Tok(Tok::Ident(k)) if k == "auto" || k == "min-content" || k == "max-content" => out.push(Track::Auto),
            Cv::Func(f, args) if f == "minmax" => {
                let parts: Vec<&[Cv]> = args.split(|c| matches!(c, Cv::Tok(Tok::Comma))).collect();
                let max = parts.get(1).and_then(|p| p.iter().find(|c| !c.is_ws()));
                match max {
                    Some(Cv::Tok(Tok::Dimension(n, unit))) if unit == "fr" => out.push(Track::Fr(*n)),
                    Some(m) => match parse_len(m, u) {
                        Some(l) => out.push(Track::Len(l)),
                        None => out.push(Track::Auto),
                    },
                    None => return false,
                }
            }
            Cv::Func(f, args) if f == "repeat" => {
                let mut parts = args.splitn(2, |c| matches!(c, Cv::Tok(Tok::Comma)));
                let count = parts.next().unwrap_or(&[]);
                let rest = parts.next().unwrap_or(&[]);
                let count_cv = count.iter().find(|c| !c.is_ws());
                match count_cv {
                    Some(Cv::Tok(Tok::Number(n))) => {
                        let n = (*n as usize).clamp(1, 64);
                        let mut one = Vec::new();
                        if !tracks(rest, u, &mut one, depth + 1) {
                            return false;
                        }
                        for _ in 0..n {
                            if out.len() + one.len() > 64 {
                                break;
                            }
                            out.extend(one.iter().copied());
                        }
                    }
                    Some(c) if matches!(c.ident(), Some("auto-fill") | Some("auto-fit")) => {
                        // minmax(<min>, 1fr) is the pattern that matters.
                        let min = rest.iter().find_map(|c| match c {
                            Cv::Func(f, a) if f == "minmax" => a.iter().find(|c| !c.is_ws()).and_then(|c| parse_len(c, u)),
                            c if !c.is_ws() => parse_len(c, u),
                            _ => None,
                        });
                        let px = match min {
                            Some(Len::Calc(px, _)) => px.max(10.0),
                            _ => 200.0,
                        };
                        out.push(Track::AutoFill(px));
                    }
                    _ => return false,
                }
            }
            Cv::Block('[', _) => {} // line names
            c => match parse_len(c, u) {
                Some(l) => out.push(Track::Len(l)),
                None => return false,
            },
        }
    }
    true
}

/// Copies one property from `from` (for inherit / initial / unset).
fn copy_prop(s: &mut Style, from: &Style, name: &str) {
    match name {
        "color" => s.color = from.color,
        "background" | "background-color" => {
            s.background = from.background;
            s.gradient = from.gradient;
        }
        "font-weight" => s.weight = from.weight,
        "font-style" => s.italic = from.italic,
        "font-family" => s.mono = from.mono,
        "line-height" => s.line_height = from.line_height,
        "text-align" => s.text_align = from.text_align,
        "text-decoration" | "text-decoration-line" => s.decoration = from.decoration,
        "text-transform" => s.transform = from.transform,
        "white-space" => s.white_space = from.white_space,
        "display" => s.display = from.display,
        "visibility" => s.hidden = from.hidden,
        "list-style" | "list-style-type" => s.list_style = from.list_style,
        "width" => s.width = from.width,
        "height" => s.height = from.height,
        "max-width" => s.max_width = from.max_width,
        "min-width" => s.min_width = from.min_width,
        "margin" => s.margin = from.margin,
        "padding" => s.padding = from.padding,
        "border" => {
            s.border_width = from.border_width;
            s.border_style = from.border_style;
            s.border_color = from.border_color;
        }
        _ => {}
    }
}

/// Applies one declaration to `s`. Unknown properties and invalid values
/// are ignored, as CSS requires.
pub fn apply(s: &mut Style, parent: &Style, name: &str, v: &[Cv], u: &Units) {
    if let Some(kw) = (values(v).len() == 1).then(|| v.iter().find_map(|c| c.ident())).flatten() {
        match kw.to_ascii_lowercase().as_str() {
            "inherit" => return copy_prop(s, parent, name),
            "initial" => return copy_prop(s, &Style::initial(), name),
            "unset" | "revert" | "revert-layer" => {
                let inherited = matches!(name, "color" | "font-weight" | "font-style" | "font-family" | "line-height" | "text-align" | "text-transform" | "white-space" | "visibility" | "list-style" | "list-style-type");
                let init = Style::initial();
                return copy_prop(s, if inherited { parent } else { &init }, name);
            }
            _ => {}
        }
    }
    let vals = values(v);
    let first = vals.first().copied();
    let id = first.and_then(|c| c.ident()).map(|s| s.to_ascii_lowercase());
    let id = id.as_deref();
    let len = |c: Option<&Cv>| c.and_then(|c| parse_len(c, u));
    match name {
        "display" => {
            let ids = idents(v);
            let has = |k: &str| ids.iter().any(|i| i == k);
            let d = if has("none") {
                Display::None
            } else if has("contents") {
                Display::Contents
            } else if has("list-item") {
                Display::ListItem
            } else if has("table") {
                if has("inline") { Display::InlineTable } else { Display::Table }
            } else if has("inline-table") {
                Display::InlineTable
            } else if has("flex") {
                if has("inline") { Display::InlineFlex } else { Display::Flex }
            } else if has("inline-flex") || has("-webkit-inline-box") {
                Display::InlineFlex
            } else if has("-webkit-box") || has("-ms-flexbox") || has("-webkit-flex") {
                Display::Flex
            } else if has("grid") {
                if has("inline") { Display::InlineGrid } else { Display::Grid }
            } else if has("inline-grid") {
                Display::InlineGrid
            } else if has("inline-block") || (has("inline") && has("flow-root")) {
                Display::InlineBlock
            } else if has("table-row-group") {
                Display::TableRowGroup
            } else if has("table-header-group") {
                Display::TableHeaderGroup
            } else if has("table-footer-group") {
                Display::TableFooterGroup
            } else if has("table-row") {
                Display::TableRow
            } else if has("table-cell") {
                Display::TableCell
            } else if has("table-caption") {
                Display::TableCaption
            } else if has("table-column") {
                Display::TableColumn
            } else if has("table-column-group") {
                Display::TableColumnGroup
            } else if has("inline") || has("ruby") || has("ruby-text") || has("ruby-base") {
                Display::Inline
            } else if has("block") || has("flow-root") || has("flow") || has("run-in") {
                Display::Block
            } else {
                return;
            };
            s.display = d;
        }
        "position" => {
            s.position = match id {
                Some("static") => Position::Static,
                Some("relative") => Position::Relative,
                Some("absolute") => Position::Absolute,
                Some("fixed") => Position::Fixed,
                Some("sticky") | Some("-webkit-sticky") => Position::Sticky,
                _ => return,
            }
        }
        "float" => {
            s.float = match id {
                Some("left") | Some("inline-start") => Float::Left,
                Some("right") | Some("inline-end") => Float::Right,
                Some("none") => Float::None,
                _ => return,
            }
        }
        "visibility" => match id {
            Some("hidden") | Some("collapse") => s.hidden = true,
            Some("visible") => s.hidden = false,
            _ => {}
        },
        "opacity" => {
            if let Some(c) = first {
                match c {
                    Cv::Tok(Tok::Number(n)) => s.opacity = n.clamp(0.0, 1.0),
                    Cv::Tok(Tok::Percentage(p)) => s.opacity = (p / 100.0).clamp(0.0, 1.0),
                    _ => {}
                }
            }
        }
        "color" => {
            if let Some(c) = first.and_then(|c| parse_color(c, parent.color)) {
                s.color = c;
            }
        }
        "background-color" => {
            if let Some(c) = first.and_then(|c| parse_color(c, s.color)) {
                s.background = c;
                s.gradient = None;
            }
        }
        "background" | "background-image" => {
            if name == "background" {
                s.background = Color::TRANSPARENT;
                s.gradient = None;
                s.has_bg_image = false;
            }
            for c in &vals {
                if let Some(g) = gradient(c, s.color) {
                    s.gradient = Some(g);
                } else if let Some(col) = parse_color(c, s.color) {
                    if name == "background" {
                        s.background = col;
                    }
                } else if matches!(c, Cv::Tok(Tok::Url(_))) || matches!(c, Cv::Func(f, _) if f == "url") {
                    s.has_bg_image = true;
                }
            }
            if name == "background-image" && id == Some("none") {
                s.gradient = None;
                s.has_bg_image = false;
            }
        }
        "font-weight" => {
            s.weight = match first {
                Some(Cv::Tok(Tok::Number(n))) => n.clamp(1.0, 1000.0) as u16,
                _ => match id {
                    Some("normal") => 400,
                    Some("bold") => 700,
                    Some("bolder") => {
                        if parent.weight < 400 {
                            400
                        } else if parent.weight < 600 {
                            700
                        } else {
                            900
                        }
                    }
                    Some("lighter") => {
                        if parent.weight < 600 {
                            100
                        } else if parent.weight < 800 {
                            400
                        } else {
                            700
                        }
                    }
                    _ => return,
                },
            }
        }
        "font-style" => match id {
            Some("italic") | Some("oblique") => s.italic = true,
            Some("normal") => s.italic = false,
            _ => {}
        },
        "font-family" => s.mono = is_mono_family(v),
        "font" => {
            // [style] [variant] [weight] size[/line-height] family
            let mut seen_size = false;
            s.italic = false;
            s.weight = 400;
            s.line_height = LineHeight::Normal;
            let mut i = 0;
            while i < v.len() {
                let c = &v[i];
                i += 1;
                if c.is_ws() {
                    continue;
                }
                if !seen_size {
                    match c {
                        Cv::Tok(Tok::Ident(k)) => match k.to_ascii_lowercase().as_str() {
                            "italic" | "oblique" => s.italic = true,
                            "bold" | "bolder" => s.weight = 700,
                            "lighter" => s.weight = 300,
                            "normal" | "small-caps" => {}
                            k if k.contains("small") || k.contains("large") || k == "medium" => seen_size = true,
                            // A system font keyword, or the family already.
                            _ => {
                                s.mono = is_mono_family(&v[i - 1..]);
                                return;
                            }
                        },
                        Cv::Tok(Tok::Number(n)) => s.weight = n.clamp(1.0, 1000.0) as u16,
                        Cv::Tok(Tok::Dimension(..)) | Cv::Tok(Tok::Percentage(..)) | Cv::Func(..) => seen_size = true,
                        _ => {}
                    }
                    if seen_size {
                        // Optional "/ line-height".
                        let mut j = i;
                        while j < v.len() && v[j].is_ws() {
                            j += 1;
                        }
                        if matches!(v.get(j), Some(Cv::Tok(Tok::Delim('/')))) {
                            j += 1;
                            while j < v.len() && v[j].is_ws() {
                                j += 1;
                            }
                            if let Some(lh) = v.get(j).and_then(|c| line_height(c, u)) {
                                s.line_height = lh;
                            }
                            i = j + 1;
                        }
                        s.mono = is_mono_family(&v[i.min(v.len())..]);
                        return;
                    }
                }
            }
        }
        "line-height" => {
            if let Some(lh) = first.and_then(|c| line_height(c, u)) {
                s.line_height = lh;
            }
        }
        "text-align" => {
            s.text_align = match id {
                Some("left") | Some("start") | Some("-webkit-left") => TextAlign::Left,
                Some("right") | Some("end") | Some("-webkit-right") => TextAlign::Right,
                Some("center") | Some("-webkit-center") | Some("-moz-center") => TextAlign::Center,
                Some("justify") => TextAlign::Justify,
                _ => return,
            }
        }
        "text-decoration" | "text-decoration-line" => {
            let mut d = 0;
            for c in &vals {
                match c.ident().map(|s| s.to_ascii_lowercase()).as_deref() {
                    Some("underline") => d |= DECO_UNDERLINE,
                    Some("overline") => d |= DECO_OVERLINE,
                    Some("line-through") => d |= DECO_LINE_THROUGH,
                    Some("none") => d = 0,
                    _ => {
                        if let Some(col) = parse_color(c, s.color) {
                            s.decoration_color = Some(col);
                        }
                    }
                }
            }
            s.decoration = d;
        }
        "text-decoration-color" => {
            if let Some(col) = first.and_then(|c| parse_color(c, s.color)) {
                s.decoration_color = Some(col);
            }
        }
        "text-transform" => {
            s.transform = match id {
                Some("uppercase") => TextTransform::Upper,
                Some("lowercase") => TextTransform::Lower,
                Some("capitalize") => TextTransform::Capitalize,
                Some("none") => TextTransform::None,
                _ => return,
            }
        }
        "text-indent" => {
            if let Some(l) = len(first) {
                s.indent = l;
            }
        }
        "white-space" | "white-space-collapse" | "text-wrap-mode" => {
            s.white_space = match (name, id) {
                (_, Some("normal")) => WhiteSpace::Normal,
                (_, Some("pre")) => WhiteSpace::Pre,
                ("white-space", Some("nowrap")) | (_, Some("nowrap")) => WhiteSpace::NoWrap,
                (_, Some("pre-wrap")) | (_, Some("break-spaces")) | (_, Some("preserve")) => WhiteSpace::PreWrap,
                (_, Some("pre-line")) | (_, Some("preserve-breaks")) => WhiteSpace::PreLine,
                (_, Some("wrap")) => {
                    if s.white_space == WhiteSpace::NoWrap {
                        WhiteSpace::Normal
                    } else {
                        s.white_space
                    }
                }
                (_, Some("collapse")) => WhiteSpace::Normal,
                _ => return,
            }
        }
        "letter-spacing" => {
            if id == Some("normal") {
                s.letter_spacing = 0.0;
            } else if let Some(Len::Calc(px, 0.0)) = len(first) {
                s.letter_spacing = px.clamp(-5.0, 50.0);
            }
        }
        "list-style-type" => {
            if let Some(ls) = id.and_then(list_style_kw) {
                s.list_style = ls;
            } else if matches!(first, Some(Cv::Tok(Tok::Str(_)))) {
                s.list_style = ListStyle::None;
            }
        }
        "list-style-position" => s.list_inside = id == Some("inside"),
        "list-style" => {
            for k in idents(v) {
                if let Some(ls) = list_style_kw(&k) {
                    s.list_style = ls;
                } else if k == "inside" {
                    s.list_inside = true;
                } else if k == "outside" {
                    s.list_inside = false;
                }
            }
        }
        "vertical-align" => {
            s.valign = match id {
                Some("baseline") => VAlign::Baseline,
                Some("sub") => VAlign::Sub,
                Some("super") => VAlign::Super,
                Some("top") => VAlign::Top,
                Some("middle") => VAlign::Middle,
                Some("bottom") => VAlign::Bottom,
                Some("text-top") => VAlign::TextTop,
                Some("text-bottom") => VAlign::TextBottom,
                _ => match len(first) {
                    Some(Len::Calc(px, pct)) => VAlign::Length(px + pct * s.line_px() / 100.0),
                    _ => return,
                },
            };
        }
        "margin" | "padding" => {
            let ls: Vec<Len> = vals.iter().filter_map(|c| parse_len(c, u)).collect();
            if ls.len() != vals.len() {
                return;
            }
            if let Some(f) = four(&ls) {
                if name == "margin" {
                    s.margin = f;
                } else {
                    s.padding = f.map(|l| if matches!(l, Len::Calc(px, pct) if px < 0.0 || pct < 0.0) { Len::ZERO } else { l });
                }
            }
        }
        "margin-inline" | "margin-block" | "padding-inline" | "padding-block" => {
            let ls: Vec<Len> = vals.iter().filter_map(|c| parse_len(c, u)).collect();
            let (a, b) = match ls.len() {
                1 => (ls[0], ls[0]),
                2 => (ls[0], ls[1]),
                _ => return,
            };
            let target = if name.starts_with("margin") { &mut s.margin } else { &mut s.padding };
            if name.ends_with("inline") {
                target[3] = a;
                target[1] = b;
            } else {
                target[0] = a;
                target[2] = b;
            }
        }
        n if n.starts_with("margin-") || n.starts_with("padding-") => {
            let (prop, side) = n.split_once('-').unwrap_or(("", ""));
            if let (Some(i), Some(l)) = (side_index(side), len(first)) {
                if prop == "margin" {
                    s.margin[i] = l;
                } else if !matches!(l, Len::Calc(px, _) if px < 0.0) {
                    s.padding[i] = l;
                }
            }
        }
        "border" | "border-top" | "border-right" | "border-bottom" | "border-left" | "border-inline" | "border-block"
        | "border-inline-start" | "border-inline-end" | "border-block-start" | "border-block-end" => {
            let Some((w, st, col)) = border_parts(v, u, s.color) else { return };
            let sides: &[usize] = match name {
                "border" => &[0, 1, 2, 3],
                "border-inline" => &[1, 3],
                "border-block" => &[0, 2],
                n => match side_index(n.trim_start_matches("border-")) {
                    Some(0) => &[0],
                    Some(1) => &[1],
                    Some(2) => &[2],
                    Some(3) => &[3],
                    _ => return,
                },
            };
            for &i in sides {
                s.border_width[i] = w;
                s.border_style[i] = st;
                s.border_color[i] = col;
            }
        }
        "border-width" => {
            let ws: Vec<f32> = vals.iter().filter_map(|c| border_width_kw(c, u)).collect();
            if ws.len() == vals.len() {
                if let Some(f) = four(&ws) {
                    s.border_width = f;
                }
            }
        }
        "border-style" => {
            let ss: Vec<BorderStyle> = vals.iter().filter_map(|c| border_style_kw(c)).collect();
            if ss.len() == vals.len() {
                if let Some(f) = four(&ss) {
                    s.border_style = f;
                }
            }
        }
        "border-color" => {
            let cs: Vec<Option<Color>> = vals.iter().map(|c| parse_color(c, s.color)).collect();
            if cs.iter().all(|c| c.is_some()) {
                if let Some(f) = four(&cs) {
                    s.border_color = f;
                }
            }
        }
        n if n.starts_with("border-") && (n.ends_with("-width") || n.ends_with("-style") || n.ends_with("-color")) => {
            let mid = &n[7..n.rfind('-').unwrap_or(n.len())];
            let Some(i) = side_index(mid) else { return };
            let Some(c) = first else { return };
            if n.ends_with("-width") {
                if let Some(w) = border_width_kw(c, u) {
                    s.border_width[i] = w;
                }
            } else if n.ends_with("-style") {
                if let Some(st) = border_style_kw(c) {
                    s.border_style[i] = st;
                }
            } else if let Some(col) = parse_color(c, s.color) {
                s.border_color[i] = Some(col);
            }
        }
        "border-radius" => {
            if let Some(l) = len(first) {
                s.radius = l;
            }
        }
        "border-collapse" => s.collapse = id == Some("collapse"),
        "border-spacing" => {
            if let Some(Len::Calc(px, _)) = len(first) {
                s.spacing = px.max(0.0);
            }
        }
        "width" | "inline-size" => {
            if let Some(l) = len(first).filter(|l| *l != Len::None) {
                s.width = l;
            }
        }
        "height" | "block-size" => {
            if let Some(l) = len(first).filter(|l| *l != Len::None) {
                s.height = l;
            }
        }
        "min-width" | "min-inline-size" => {
            if let Some(l) = len(first) {
                s.min_width = l;
            }
        }
        "max-width" | "max-inline-size" => {
            if let Some(l) = len(first) {
                s.max_width = if l == Len::Auto { Len::None } else { l };
            }
        }
        "min-height" | "min-block-size" => {
            if let Some(l) = len(first) {
                s.min_height = l;
            }
        }
        "max-height" | "max-block-size" => {
            if let Some(l) = len(first) {
                s.max_height = if l == Len::Auto { Len::None } else { l };
            }
        }
        "box-sizing" => s.border_box = id == Some("border-box"),
        "overflow" | "overflow-x" | "overflow-y" => {
            if let Some(k) = id {
                s.overflow_hidden = matches!(k, "hidden" | "clip" | "auto" | "scroll");
            }
        }
        "top" | "right" | "bottom" | "left" => {
            if let (Some(i), Some(l)) = (side_index(name), len(first)) {
                s.inset[i] = l;
            }
        }
        "inset" => {
            let ls: Vec<Len> = vals.iter().filter_map(|c| parse_len(c, u)).collect();
            if let Some(f) = four(&ls) {
                s.inset = f;
            }
        }
        "clip" => {
            if let Some(Cv::Func(f, args)) = first {
                if f == "rect" {
                    let nums: Vec<f32> = args.iter().filter_map(|c| match c {
                        Cv::Tok(Tok::Dimension(n, _)) | Cv::Tok(Tok::Number(n)) => Some(*n),
                        _ => None,
                    }).collect();
                    s.clipped = nums.len() == 4 && (nums[2] - nums[0]).abs() <= 1.0 && (nums[1] - nums[3]).abs() <= 1.0;
                }
            }
        }
        "clip-path" => {
            if let Some(Cv::Func(f, args)) = first {
                if f == "inset" {
                    s.clipped = matches!(args.iter().find(|c| !c.is_ws()), Some(Cv::Tok(Tok::Percentage(p))) if *p >= 50.0);
                }
            }
        }
        "flex-direction" => {
            s.flex_column = matches!(id, Some("column") | Some("column-reverse"));
            s.flex_reverse = matches!(id, Some("row-reverse") | Some("column-reverse"));
        }
        "flex-wrap" => s.flex_wrap = matches!(id, Some("wrap") | Some("wrap-reverse")),
        "flex-flow" => {
            for k in idents(v) {
                match k.as_str() {
                    "row" | "row-reverse" => {
                        s.flex_column = false;
                        s.flex_reverse = k == "row-reverse";
                    }
                    "column" | "column-reverse" => {
                        s.flex_column = true;
                        s.flex_reverse = k == "column-reverse";
                    }
                    "wrap" | "wrap-reverse" => s.flex_wrap = true,
                    "nowrap" => s.flex_wrap = false,
                    _ => {}
                }
            }
        }
        "justify-content" => {
            let ids = idents(v);
            let k = ids.last().map(|s| s.as_str());
            s.justify = match k {
                Some("flex-start") | Some("start") | Some("left") | Some("normal") | Some("stretch") => Justify::Start,
                Some("flex-end") | Some("end") | Some("right") => Justify::End,
                Some("center") => Justify::Center,
                Some("space-between") => Justify::SpaceBetween,
                Some("space-around") => Justify::SpaceAround,
                Some("space-evenly") => Justify::SpaceEvenly,
                _ => return,
            }
        }
        "align-items" => {
            let ids = idents(v);
            s.align_items = match ids.last().map(|s| s.as_str()) {
                Some("stretch") | Some("normal") => Align::Stretch,
                Some("flex-start") | Some("start") | Some("self-start") => Align::Start,
                Some("flex-end") | Some("end") | Some("self-end") => Align::End,
                Some("center") => Align::Center,
                Some("baseline") => Align::Baseline,
                _ => return,
            }
        }
        "gap" | "grid-gap" => {
            let ls: Vec<Len> = vals.iter().filter_map(|c| parse_len(c, u)).collect();
            match ls.len() {
                1 => s.gap = (ls[0], ls[0]),
                2 => s.gap = (ls[0], ls[1]),
                _ => {}
            }
        }
        "row-gap" | "grid-row-gap" => {
            if let Some(l) = len(first) {
                s.gap.0 = l;
            }
        }
        "column-gap" | "grid-column-gap" => {
            if let Some(l) = len(first) {
                s.gap.1 = l;
            }
        }
        "flex-grow" => {
            if let Some(Cv::Tok(Tok::Number(n))) = first {
                s.grow = n.clamp(0.0, 1e4);
            }
        }
        "flex-shrink" => {
            if let Some(Cv::Tok(Tok::Number(n))) = first {
                s.shrink = n.clamp(0.0, 1e4);
            }
        }
        "flex-basis" => {
            if let Some(l) = len(first) {
                s.basis = l;
            }
        }
        "flex" => {
            match id {
                Some("none") => {
                    s.grow = 0.0;
                    s.shrink = 0.0;
                    s.basis = Len::Auto;
                    return;
                }
                Some("auto") => {
                    s.grow = 1.0;
                    s.shrink = 1.0;
                    s.basis = Len::Auto;
                    return;
                }
                _ => {}
            }
            let mut nums = Vec::new();
            let mut basis = None;
            for c in &vals {
                match c {
                    Cv::Tok(Tok::Number(n)) => nums.push(*n),
                    c => basis = parse_len(c, u),
                }
            }
            s.grow = nums.first().copied().unwrap_or(1.0).clamp(0.0, 1e4);
            s.shrink = nums.get(1).copied().unwrap_or(1.0).clamp(0.0, 1e4);
            s.basis = basis.unwrap_or(if nums.is_empty() { Len::Auto } else { Len::ZERO });
        }
        "order" => {
            if let Some(Cv::Tok(Tok::Number(n))) = first {
                s.order = n.clamp(-1000.0, 1000.0) as i32;
            }
        }
        "grid-template-columns" | "grid-template" => {
            let mut t = Vec::new();
            let part: &[Cv] = if name == "grid-template" {
                match v.iter().position(|c| matches!(c, Cv::Tok(Tok::Delim('/')))) {
                    Some(i) => &v[i + 1..],
                    None => return,
                }
            } else {
                v
            };
            if tracks(part, u, &mut t, 0) {
                s.grid_columns = t;
            }
        }
        "grid-column" | "grid-column-end" => {
            // "span 2" / "1 / -1" (full width) / "1 / 3"
            let ids = idents(v);
            if ids.first().map(|s| s.as_str()) == Some("span") {
                if let Some(Cv::Tok(Tok::Number(n))) = vals.get(1) {
                    s.grid_span = (*n as u16).clamp(1, 64);
                }
            } else {
                let nums: Vec<f32> = vals.iter().filter_map(|c| if let Cv::Tok(Tok::Number(n)) = c { Some(*n) } else { None }).collect();
                if nums.len() == 2 {
                    s.grid_span = if nums[1] < 0.0 { 64 } else { ((nums[1] - nums[0]).abs() as u16).clamp(1, 64) };
                }
            }
        }
        "content" => {
            let mut out = String::new();
            for c in &vals {
                match c {
                    Cv::Tok(Tok::Str(t)) => out.push_str(t),
                    Cv::Tok(Tok::Ident(k)) if k == "none" || k == "normal" => {
                        s.content = None;
                        return;
                    }
                    Cv::Tok(Tok::Ident(k)) if k == "open-quote" => out.push('\u{201C}'),
                    Cv::Tok(Tok::Ident(k)) if k == "close-quote" => out.push('\u{201D}'),
                    _ => {}
                }
            }
            // Icon-font glyphs (private use area) mean nothing without the font.
            if out.chars().any(|c| ('\u{E000}'..='\u{F8FF}').contains(&c)) {
                out.clear();
            }
            s.content = Some(out);
        }
        _ => {}
    }
}

fn line_height(c: &Cv, u: &Units) -> Option<LineHeight> {
    Some(match c {
        Cv::Tok(Tok::Ident(k)) if k.eq_ignore_ascii_case("normal") => LineHeight::Normal,
        Cv::Tok(Tok::Number(n)) => LineHeight::Number(n.clamp(0.0, 20.0)),
        Cv::Tok(Tok::Percentage(p)) => LineHeight::Px(u.em * p / 100.0),
        Cv::Tok(Tok::Dimension(n, unit)) => LineHeight::Px(unit_px(*n, unit, u)?.clamp(0.0, 2000.0)),
        Cv::Func(..) => match parse_len(c, u)? {
            Len::Calc(px, pct) => LineHeight::Px(px + u.em * pct / 100.0),
            _ => return None,
        },
        _ => return None,
    })
}

/// True if the first font family that names a generic or a well-known face
/// is monospaced.
fn is_mono_family(v: &[Cv]) -> bool {
    for c in v {
        let name = match c {
            Cv::Tok(Tok::Ident(s)) | Cv::Tok(Tok::Str(s)) => s.to_ascii_lowercase(),
            _ => continue,
        };
        if name.contains("mono") || name.contains("courier") || name.contains("consolas") || name.contains("menlo") || name == "monaco" || name.contains("code") || name.contains("typewriter") {
            return true;
        }
        if ["serif", "sans-serif", "system-ui", "arial", "helvetica", "georgia", "times", "verdana", "-apple-system", "inter", "roboto", "ui-sans-serif", "ui-serif"].contains(&name.as_str()) {
            return false;
        }
    }
    false
}

/// HTML presentational attributes, as declarations.
fn presentational_hints(doc: &Document, n: NodeId) -> Vec<Declaration> {
    let Some(e) = doc.element(n) else { return Vec::new() };
    let mut css = String::new();
    let name = e.name.as_str();
    let dim = |v: &str| -> Option<String> {
        let v = v.trim();
        let num: String = v.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
        if num.is_empty() {
            return None;
        }
        Some(if v[num.len()..].starts_with('%') { format!("{num}%") } else { format!("{num}px") })
    };
    let color = |v: &str| -> String {
        let v = v.trim();
        if v.len() == 6 && v.bytes().all(|b| b.is_ascii_hexdigit()) {
            format!("#{v}")
        } else {
            v.to_string()
        }
    };
    for (k, v) in &e.attrs {
        match (name, k.as_str()) {
            (_, "bgcolor") => css += &format!("background-color:{};", color(v)),
            ("body", "text") | ("font", "color") => css += &format!("color:{};", color(v)),
            ("img" | "table" | "td" | "th" | "col" | "hr" | "iframe" | "embed" | "object" | "video" | "canvas" | "svg" | "input", "width") => {
                if let Some(d) = dim(v) {
                    css += &format!("width:{d};");
                }
            }
            ("img" | "td" | "th" | "tr" | "table" | "iframe" | "embed" | "object" | "video" | "canvas" | "svg", "height") => {
                if let Some(d) = dim(v) {
                    css += &format!("height:{d};");
                }
            }
            ("td" | "th" | "tr" | "div" | "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "thead" | "tbody" | "tfoot" | "caption", "align") => {
                let a = v.to_ascii_lowercase();
                if ["left", "right", "center", "justify"].contains(&a.as_str()) {
                    css += &format!("text-align:{a};");
                }
            }
            ("table", "align") if v.eq_ignore_ascii_case("center") => css += "margin-left:auto;margin-right:auto;",
            ("img" | "table", "align") if v.eq_ignore_ascii_case("left") || v.eq_ignore_ascii_case("right") => {
                css += &format!("float:{};", v.to_ascii_lowercase())
            }
            ("td" | "th" | "tr" | "tbody" | "thead", "valign") => css += &format!("vertical-align:{};", v.to_ascii_lowercase()),
            ("td" | "th", "nowrap") => css += "white-space:nowrap;",
            ("table", "border") => {
                let b = v.trim().parse::<f32>().unwrap_or(1.0);
                if b > 0.0 {
                    css += &format!("border:{b}px outset #808080;");
                }
            }
            ("table", "cellspacing") => {
                if let Some(d) = dim(v) {
                    css += &format!("border-spacing:{d};");
                }
            }
            ("font", "size") => {
                let t = v.trim();
                let base = 3i32;
                let n = if let Some(r) = t.strip_prefix('+') {
                    base + r.parse::<i32>().unwrap_or(0)
                } else if let Some(r) = t.strip_prefix('-') {
                    base - r.parse::<i32>().unwrap_or(0)
                } else {
                    t.parse().unwrap_or(3)
                };
                let size = ["x-small", "x-small", "small", "medium", "large", "x-large", "xx-large", "xxx-large"][n.clamp(1, 7) as usize];
                css += &format!("font-size:{size};");
            }
            ("font", "face") => css += &format!("font-family:{};", v.replace([';', '{', '}'], "")),
            ("ol" | "ul" | "li", "type") => {
                let t = match v.as_str() {
                    "1" => "decimal",
                    "a" => "lower-alpha",
                    "A" => "upper-alpha",
                    "i" => "lower-roman",
                    "I" => "upper-roman",
                    "disc" | "circle" | "square" => v.as_str(),
                    _ => continue,
                };
                css += &format!("list-style-type:{t};");
            }
            ("img", "border") | ("object", "border") => {
                if let Some(d) = dim(v) {
                    css += &format!("border:{d} solid;");
                }
            }
            ("img", "hspace") => {
                if let Some(d) = dim(v) {
                    css += &format!("margin-left:{d};margin-right:{d};");
                }
            }
            ("img", "vspace") => {
                if let Some(d) = dim(v) {
                    css += &format!("margin-top:{d};margin-bottom:{d};");
                }
            }
            ("hr", "size") => {
                if let Some(d) = dim(v) {
                    css += &format!("height:{d};");
                }
            }
            ("hr", "noshade") => css += "border-style:solid;background-color:gray;",
            ("hr", "color") => css += &format!("border-color:{0};background-color:{0};", color(v)),
            _ => {}
        }
    }
    // cellpadding applies to the table's cells.
    if name == "td" || name == "th" {
        let mut t = doc.parent(n);
        while let Some(p) = t {
            if doc.is(p, "table") {
                if let Some(e) = doc.element(p) {
                    if let Some(cp) = e.attr("cellpadding").and_then(dim) {
                        css = format!("padding:{cp};") + &css;
                    }
                    if e.attr("border").is_some_and(|b| b.trim().parse::<f32>().unwrap_or(1.0) > 0.0) {
                        css = "border:1px inset #808080;".to_string() + &css;
                    }
                }
                break;
            }
            t = doc.parent(p);
        }
    }
    if css.is_empty() {
        return Vec::new();
    }
    super::parser::parse_declarations(&component_values(tokenize(&css)))
}

/// Text of a node (for tests and titles).
pub fn is_text(doc: &Document, n: NodeId) -> bool {
    matches!(doc.nodes[n].data, NodeData::Text(_))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::html::parse;

    fn styles(html: &str, css: &str) -> (Document, Styles) {
        let d = parse(html);
        let env = MediaEnv::default();
        let sheet = Stylesheet::parse(css, &env);
        let s = compute_styles(&d, &[sheet], &env);
        (d, s)
    }

    #[test]
    fn cascade_order_and_specificity() {
        let (d, s) = styles(
            "<p id=x class=c style='margin-top: 3px'>t</p><p class=c>u</p>",
            "p { color: red } .c { color: blue } #x { color: green } p { color: yellow !important } .c { margin-top: 9px !important }",
        );
        let ps = d.find_all("p");
        let a = s.get(ps[0]).unwrap();
        let b = s.get(ps[1]).unwrap();
        assert_eq!(a.color, Color::rgb(255, 255, 0)); // !important beats the id
        assert_eq!(b.color, Color::rgb(255, 255, 0));
        assert_eq!(a.margin[0], Len::px(9.0)); // author !important beats inline normal
    }

    #[test]
    fn inheritance_and_units() {
        let (d, s) = styles(
            "<div><p>a <em>b</em></p><h1>h</h1><small>s</small></div>",
            "div { font-size: 20px; color: #123456; margin: 1em } p { font-size: 1.5em; padding: 2em 10% }",
        );
        let p = s.get(d.find("p").unwrap()).unwrap();
        assert_eq!(p.font_size, 30.0);
        assert_eq!(p.padding[0], Len::px(60.0));
        assert_eq!(p.padding[1], Len::pct(10.0));
        let em = s.get(d.find("em").unwrap()).unwrap();
        assert!(em.italic);
        assert_eq!(em.color, Color::hex(0x123456));
        assert_eq!(s.get(d.find("h1").unwrap()).unwrap().font_size, 40.0);
        assert!(s.get(d.find("h1").unwrap()).unwrap().bold());
        let div = s.get(d.find("div").unwrap()).unwrap();
        assert_eq!(div.margin[3], Len::px(20.0));
        assert_eq!(div.display, Display::Block);
    }

    #[test]
    fn ua_defaults() {
        let (d, s) = styles("<ul><li>a</li></ul><ol><li>b</li></ol><a href=x>l</a><script>x</script><table><tr><td>c</td></tr></table>", "");
        let li = s.get(d.find("li").unwrap()).unwrap();
        assert_eq!(li.display, Display::ListItem);
        assert_eq!(li.list_style, ListStyle::Disc);
        assert_eq!(s.get(d.find_all("li")[1]).unwrap().list_style, ListStyle::Decimal);
        let a = s.get(d.find("a").unwrap()).unwrap();
        assert!(a.decoration & DECO_UNDERLINE != 0);
        assert_eq!(s.get(d.find("script").unwrap()).unwrap().display, Display::None);
        assert_eq!(s.get(d.find("head").unwrap()).unwrap().display, Display::None);
        assert_eq!(s.get(d.find("td").unwrap()).unwrap().display, Display::TableCell);
        assert_eq!(s.get(d.find("body").unwrap()).unwrap().margin[0], Len::px(8.0));
    }

    #[test]
    fn custom_properties() {
        let (d, s) = styles(
            "<div><p>x</p></div>",
            ":root { --fg: #ff0000; --pad: 4px; --loop: var(--loop) } div { color: var(--fg); padding: var(--pad) var(--missing, 7px) } p { margin: var(--loop, 2px); background: var(--nope) }",
        );
        let div = s.get(d.find("div").unwrap()).unwrap();
        assert_eq!(div.color, Color::rgb(255, 0, 0));
        assert_eq!(div.padding[0], Len::px(4.0));
        assert_eq!(div.padding[1], Len::px(7.0));
        let p = s.get(d.find("p").unwrap()).unwrap();
        assert_eq!(p.margin[0], Len::px(16.0)); // cycle: invalid, so the UA margin stays
    }

    #[test]
    fn presentational_attributes() {
        let (d, s) = styles("<table border=1 cellpadding=5 width=80%><tr><td bgcolor=ff0000 align=right>x</td></tr></table><font size=5 color=green>f</font>", "");
        let td = s.get(d.find("td").unwrap()).unwrap();
        assert_eq!(td.background, Color::rgb(255, 0, 0));
        assert_eq!(td.text_align, TextAlign::Right);
        assert_eq!(td.padding[0], Len::px(5.0));
        let t = s.get(d.find("table").unwrap()).unwrap();
        assert_eq!(t.width, Len::pct(80.0));
        let f = s.get(d.find("font").unwrap()).unwrap();
        assert_eq!(f.font_size, 24.0);
        assert_eq!(f.color, Color::rgb(0, 128, 0));
    }

    #[test]
    fn shorthands() {
        let (d, s) = styles(
            "<div>x</div>",
            "div { border: 2px dashed #00f; border-left: none; font: italic bold 12px/1.5 Menlo, monospace; background: #eee linear-gradient(to right, red, blue); flex: 1 0 100px; display: flex }",
        );
        let x = s.get(d.find("div").unwrap()).unwrap();
        assert_eq!(x.border(0), 2.0);
        assert_eq!(x.border(3), 0.0);
        assert_eq!(x.border_style[1], BorderStyle::Dashed);
        assert_eq!(x.border_color(1), Color::rgb(0, 0, 255));
        assert!(x.italic && x.bold() && x.mono);
        assert_eq!(x.font_size, 12.0);
        assert_eq!(x.line_height, LineHeight::Number(1.5));
        assert_eq!(x.background, Color::hex(0xEEEEEE));
        assert_eq!(x.gradient, Some((Color::rgb(255, 0, 0), Color::rgb(0, 0, 255), false)));
        assert_eq!((x.grow, x.shrink, x.basis), (1.0, 0.0, Len::px(100.0)));
        assert_eq!(x.display, Display::Flex);
    }

    #[test]
    fn hostile_var_expansion() {
        // Each level doubles: bounded by the substitution budget.
        let mut css = String::from(":root { --a0: x; ");
        for i in 1..40 {
            css += &format!("--a{i}: var(--a{}) var(--a{}); ", i - 1, i - 1);
        }
        css += "} p { margin: var(--a39) }";
        let t = std::time::Instant::now();
        let _ = styles("<p>x</p>", &css);
        assert!(t.elapsed().as_secs() < 2);
    }
}
