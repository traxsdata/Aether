//! layout - from styled DOM to positioned boxes, as a display list.
//!
//! `build` turns the DOM and computed styles into a box tree (block
//! containers, inline formatting contexts, tables, flex and grid
//! containers, replaced elements). `flow` lays the tree out for a viewport
//! width: block flow with margin collapsing between siblings, line breaking
//! and alignment, shrink-to-fit inline blocks, an automatic table layout,
//! single-line and wrapping flex rows, simple grids. The result is a flat
//! display list in page coordinates plus the hit regions (links, form
//! fields) and anchor positions the UI needs.

pub mod build;
pub mod flow;
pub mod inline;

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::css::style::{BorderStyle, Style};
use crate::css::Color;
use crate::dom::NodeId;
use crate::font::Face;

pub use build::{build_tree, BuildOutput};
pub use flow::layout_document;

/// Gives replaced boxes their picture's intrinsic size before layout.
/// `sizes` maps an `<img>` node to its decoded width and height.
pub fn apply_image_sizes(root: &LBox, sizes: &HashMap<NodeId, (u32, u32)>) {
    fn walk(b: &LBox, sizes: &HashMap<NodeId, (u32, u32)>) {
        if let Kind::Replaced(Replaced::Image { .. }) = &b.kind {
            if let Some(n) = b.node {
                if let Some(&(w, h)) = sizes.get(&n) {
                    b.intrinsic.set(Some((w as f32, h as f32)));
                }
            }
        }
        for c in &b.children {
            walk(c, sizes);
        }
        for i in &b.inlines {
            if let Inl::Atomic(c, _) = i {
                walk(c, sizes);
            }
        }
        if let Kind::Table(t) = &b.kind {
            if let Some(c) = &t.caption {
                walk(c, sizes);
            }
            for row in &t.rows {
                for cell in &row.cells {
                    walk(&cell.b, sizes);
                }
            }
        }
    }
    walk(root, sizes);
}

/// Style of a run of text, flattened from its inline ancestors.
#[derive(Clone, Debug, PartialEq)]
pub struct TextStyle {
    pub face: Face,
    pub color: Color,
    pub deco: u8,
    pub deco_color: Option<Color>,
    pub letter_spacing: f32,
    /// Baseline shift (sub/super), positive is up.
    pub shift: f32,
    pub bg: Option<Color>,
    pub line_h: f32,
    pub ws: crate::css::style::WhiteSpace,
    pub link: Option<u32>,
    pub hidden: bool,
    pub node: Option<NodeId>,
    /// The innermost inline element the text is in.
    pub elem: Option<NodeId>,
}

#[derive(Clone, Debug)]
pub enum Inl {
    Text(Rc<TextStyle>, String),
    /// An inline box opens: its left margin + border + padding.
    Open(Rc<TextStyle>, f32, Option<NodeId>),
    /// It closes: right margin + border + padding.
    Close(f32),
    Atomic(Box<LBox>, Option<u32>),
    Br(Rc<TextStyle>),
}

#[derive(Clone, Debug)]
pub enum Marker {
    Bullet(crate::css::style::ListStyle),
    Text(String),
}

#[derive(Clone, Debug)]
pub enum Replaced {
    /// An image (or other media) that isn't shown: a placeholder.
    Image { alt: String, src: String },
    /// A form control.
    Field(FieldKind),
    /// An embedded frame, not loaded; links to its address.
    Frame { src: String },
    /// SVG and MathML: space is kept, nothing is drawn.
    Blank,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FieldKind {
    Text { placeholder: String, password: bool },
    TextArea { placeholder: String },
    Button { label: String, submit: bool },
    Check { radio: bool, checked: bool },
    Select { label: String },
}

#[derive(Clone, Debug)]
pub struct Cell_ {
    pub b: LBox,
    pub colspan: usize,
    pub rowspan: usize,
}

#[derive(Clone, Debug)]
pub struct Row {
    pub style: Rc<Style>,
    pub cells: Vec<Cell_>,
}

#[derive(Clone, Debug)]
pub struct TableData {
    pub caption: Option<Box<LBox>>,
    pub rows: Vec<Row>,
}

#[derive(Clone, Debug)]
pub enum Kind {
    /// A block container: block children, or (if it has none) inline content.
    Block,
    Table(TableData),
    Flex,
    Grid,
    Replaced(Replaced),
}

#[derive(Clone, Debug)]
pub struct LBox {
    pub style: Rc<Style>,
    pub node: Option<NodeId>,
    pub kind: Kind,
    pub children: Vec<LBox>,
    pub inlines: Vec<Inl>,
    pub marker: Option<Marker>,
    pub(crate) intrinsic: Cell<Option<(f32, f32)>>,
}

impl LBox {
    pub fn new(style: Rc<Style>, node: Option<NodeId>, kind: Kind) -> LBox {
        LBox { style, node, kind, children: Vec::new(), inlines: Vec::new(), marker: None, intrinsic: Cell::new(None) }
    }
}

/// What a click on a region does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    Link(u32),
    Field(NodeId),
    Submit(NodeId),
    Check(NodeId),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Empty,
    /// A box's background and border.
    Deco {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        bg: Color,
        gradient: Option<(Color, Color, bool)>,
        radius: f32,
        border: [f32; 4],
        colors: [Color; 4],
        styles: [BorderStyle; 4],
    },
    Rect { x: f32, y: f32, w: f32, h: f32, color: Color, radius: f32 },
    Text { x: f32, y: f32, text: String, face: Face, color: Color, deco: u8, deco_color: Color, letter_spacing: f32 },
    Bullet { x: f32, y: f32, size: f32, kind: u8, color: Color },
    Image { x: f32, y: f32, w: f32, h: f32, alt: String, node: Option<NodeId> },
    /// A form field whose current value the painter looks up.
    Field { x: f32, y: f32, w: f32, h: f32, node: NodeId, kind: FieldKind, face: Face, color: Color },
    ClipPush { x: f32, y: f32, w: f32, h: f32 },
    ClipPop,
    Hit { x: f32, y: f32, w: f32, h: f32, target: Target },
    Anchor { y: f32, node: NodeId },
    /// Where an element (or a run of an inline element's text) was put;
    /// not drawn. Scripts use these for clicks and element sizes.
    Region { x: f32, y: f32, w: f32, h: f32, node: NodeId },
}

impl Item {
    pub fn shift(&mut self, dx: f32, dy: f32) {
        match self {
            Item::Deco { x, y, .. }
            | Item::Rect { x, y, .. }
            | Item::Text { x, y, .. }
            | Item::Bullet { x, y, .. }
            | Item::Image { x, y, .. }
            | Item::Field { x, y, .. }
            | Item::ClipPush { x, y, .. }
            | Item::Hit { x, y, .. }
            | Item::Region { x, y, .. } => {
                *x += dx;
                *y += dy;
            }
            Item::Anchor { y, .. } => *y += dy,
            Item::Empty | Item::ClipPop => {}
        }
    }

    /// Vertical extent, for culling (None: always processed).
    pub fn y_range(&self) -> Option<(f32, f32)> {
        match self {
            Item::Deco { y, h, .. } | Item::Rect { y, h, .. } | Item::Image { y, h, .. } | Item::Field { y, h, .. } | Item::Hit { y, h, .. } | Item::Region { y, h, .. } => {
                Some((*y, *y + *h))
            }
            Item::Text { y, face, .. } => Some((*y - face.ascent() - 4.0, *y + face.descent() + 4.0)),
            Item::Bullet { y, size, .. } => Some((*y - size, *y + size)),
            _ => None,
        }
    }
}

/// A laid-out page.
#[derive(Clone, Debug, Default)]
pub struct Page {
    pub items: Vec<Item>,
    pub width: f32,
    pub height: f32,
    pub background: Color,
    /// Link targets (raw href values), indexed by Target::Link.
    pub links: Vec<String>,
}

impl Page {
    /// The topmost hit region at (x, y) in page coordinates.
    pub fn hit(&self, x: f32, y: f32) -> Option<Target> {
        self.items.iter().rev().find_map(|it| match it {
            Item::Hit { x: hx, y: hy, w, h, target } if x >= *hx && x < hx + w && y >= *hy && y < hy + h => Some(*target),
            _ => None,
        })
    }

    /// The innermost element at (x, y) in page coordinates.
    pub fn node_at(&self, x: f32, y: f32) -> Option<NodeId> {
        self.items.iter().rev().find_map(|it| match it {
            Item::Region { x: rx, y: ry, w, h, node } if x >= *rx && x < rx + w && y >= *ry && y < ry + h => Some(*node),
            _ => None,
        })
    }

    /// Each element's bounding box (the union of its regions).
    pub fn node_rects(&self) -> std::collections::HashMap<NodeId, [f32; 4]> {
        let mut m: std::collections::HashMap<NodeId, [f32; 4]> = std::collections::HashMap::new();
        for it in &self.items {
            if let Item::Region { x, y, w, h, node } = it {
                m.entry(*node)
                    .and_modify(|r| {
                        let (x0, y0) = (r[0].min(*x), r[1].min(*y));
                        let (x1, y1) = ((r[0] + r[2]).max(x + w), (r[1] + r[3]).max(y + h));
                        *r = [x0, y0, x1 - x0, y1 - y0];
                    })
                    .or_insert([*x, *y, *w, *h]);
            }
        }
        m
    }

    pub fn anchor_y(&self, node: NodeId) -> Option<f32> {
        self.items.iter().find_map(|it| match it {
            Item::Anchor { y, node: n } if *n == node => Some(*y),
            _ => None,
        })
    }

    /// All text, in display order (for tests).
    pub fn text(&self) -> String {
        let mut s = String::new();
        for it in &self.items {
            if let Item::Text { text, .. } = it {
                if !s.is_empty() {
                    s.push(' ');
                }
                s.push_str(text);
            }
        }
        s
    }
}
