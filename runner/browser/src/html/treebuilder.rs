//! treebuilder.rs - HTML tree construction (WHATWG "13.2.6", condensed).
//!
//! Implements the insertion modes that shape real pages: implied html, head
//! and body; head content; implied end tags and auto-closing (p, li, dd/dt,
//! headings, options); the list of active formatting elements with its
//! reconstruction and the adoption agency algorithm (so `<b><p>x</b>y` and
//! misnested tags come out as browsers build them); tables with implied
//! tbody/tr, cell closing and foster parenting; select; frameset; and svg /
//! math as opaque foreign subtrees. Scripting is off, so <noscript> content
//! is ordinary markup.
//!
//! Limits: at most MAX_DEPTH open elements (deeper elements become leaves
//! of the deepest one), at most MAX_NODES nodes, a bounded list of active
//! formatting elements. Every loop is bounded by those sizes, so hostile
//! input can't make it slow or deep.

use super::tokenizer::{Tag, Token, Tokenizer};
use crate::dom::{Document, Element, NodeData, NodeId};

pub const MAX_DEPTH: usize = 200;
pub const MAX_NODES: usize = 250_000;
const MAX_AFE: usize = 48;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Initial,
    BeforeHtml,
    BeforeHead,
    InHead,
    InHeadNoscript,
    AfterHead,
    InBody,
    Text,
    InTable,
    InCaption,
    InColumnGroup,
    InTableBody,
    InRow,
    InCell,
    InSelect,
    InSelectInTable,
    AfterBody,
    InFrameset,
    AfterFrameset,
    AfterAfterBody,
}

#[derive(Clone, Debug)]
enum Afe {
    Marker,
    El(NodeId, Tag),
}

pub struct TreeBuilder {
    doc: Document,
    tok: Tokenizer,
    open: Vec<NodeId>,
    afe: Vec<Afe>,
    head: Option<NodeId>,
    form: Option<NodeId>,
    mode: Mode,
    orig_mode: Mode,
    foster: bool,
    /// Drop a newline right after <pre>, <listing> or <textarea>.
    skip_lf: bool,
    full: bool,
}

const SPECIAL: &[&str] = &[
    "address", "applet", "area", "article", "aside", "base", "basefont", "bgsound", "blockquote", "body", "br", "button",
    "caption", "center", "col", "colgroup", "dd", "details", "dir", "div", "dl", "dt", "embed", "fieldset", "figcaption",
    "figure", "footer", "form", "frame", "frameset", "h1", "h2", "h3", "h4", "h5", "h6", "head", "header", "hgroup", "hr",
    "html", "iframe", "img", "input", "keygen", "li", "link", "listing", "main", "marquee", "menu", "meta", "nav",
    "noembed", "noframes", "noscript", "object", "ol", "p", "param", "plaintext", "pre", "script", "search", "section",
    "select", "source", "style", "summary", "table", "tbody", "td", "template", "textarea", "tfoot", "th", "thead",
    "title", "tr", "track", "ul", "wbr", "xmp",
];
const BLOCKISH: &[&str] = &[
    "address", "article", "aside", "blockquote", "center", "details", "dialog", "dir", "div", "dl", "fieldset",
    "figcaption", "figure", "footer", "header", "hgroup", "main", "menu", "nav", "ol", "p", "search", "section",
    "summary", "ul",
];
const FORMATTING: &[&str] = &["a", "b", "big", "code", "em", "font", "i", "nobr", "s", "small", "strike", "strong", "tt", "u"];
const IMPLIED_END: &[&str] = &["dd", "dt", "li", "optgroup", "option", "p", "rb", "rp", "rt", "rtc"];
const HEADINGS: &[&str] = &["h1", "h2", "h3", "h4", "h5", "h6"];
const BREAKOUT: &[&str] = &[
    "b", "big", "blockquote", "body", "br", "center", "code", "dd", "div", "dl", "dt", "em", "embed", "h1", "h2", "h3",
    "h4", "h5", "h6", "head", "hr", "i", "img", "li", "listing", "menu", "meta", "nobr", "ol", "p", "pre", "ruby", "s",
    "small", "span", "strong", "strike", "sub", "sup", "table", "tt", "u", "ul", "var",
];

fn is_ws_text(s: &str) -> bool {
    s.bytes().all(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\x0C' | b'\r'))
}

/// Splits leading white space off a text token.
fn split_ws(s: &str) -> (&str, &str) {
    let i = s.find(|c: char| !matches!(c, ' ' | '\t' | '\n' | '\x0C' | '\r')).unwrap_or(s.len());
    s.split_at(i)
}

fn start(name: &str) -> Tag {
    Tag { name: name.into(), attrs: Vec::new(), self_closing: false }
}

pub fn parse(html: &str) -> Document {
    let mut b = TreeBuilder {
        doc: Document::default(),
        tok: Tokenizer::new(html),
        open: Vec::new(),
        afe: Vec::new(),
        head: None,
        form: None,
        mode: Mode::Initial,
        orig_mode: Mode::InBody,
        foster: false,
        skip_lf: false,
        full: false,
    };
    loop {
        let t = b.tok.next_token();
        let eof = t == Token::Eof;
        let t = match t {
            Token::Text(s) if b.skip_lf => {
                b.skip_lf = false;
                match s.strip_prefix('\n') {
                    Some("") => continue,
                    Some(rest) => Token::Text(rest.to_string()),
                    None => Token::Text(s),
                }
            }
            t => {
                b.skip_lf = false;
                t
            }
        };
        b.dispatch(t);
        if eof || b.full {
            break;
        }
    }
    b.finish()
}

impl TreeBuilder {
    fn finish(mut self) -> Document {
        // A document always has html, head and body.
        if self.doc.html_element().is_none() {
            let html = self.create(&start("html"));
            self.doc.append(Document::ROOT, html);
        }
        let html = self.doc.html_element().unwrap_or(0);
        if !self.doc.child_elements(html).any(|c| self.doc.is(c, "head")) {
            let head = self.create(&start("head"));
            let first = self.doc.children(html).first().copied();
            match first {
                Some(f) => self.doc.insert_before(html, head, f),
                None => self.doc.append(html, head),
            }
        }
        if !self.doc.child_elements(html).any(|c| self.doc.is(c, "body") || self.doc.is(c, "frameset")) {
            let body = self.create(&start("body"));
            self.doc.append(html, body);
        }
        self.doc
    }

    // ---- tree helpers ---------------------------------------------------

    fn current(&self) -> NodeId {
        self.open.last().copied().unwrap_or(Document::ROOT)
    }

    fn cur_is(&self, name: &str) -> bool {
        self.open.last().is_some_and(|&n| self.doc.is(n, name))
    }

    fn cur_in(&self, names: &[&str]) -> bool {
        self.open.last().is_some_and(|&n| self.is_html(n) && names.contains(&self.doc.name(n)))
    }

    fn is_html(&self, n: NodeId) -> bool {
        self.doc.element(n).is_some_and(|e| !e.foreign)
    }

    fn is_foreign(&self, n: NodeId) -> bool {
        self.doc.element(n).is_some_and(|e| e.foreign)
    }

    fn is_special(&self, n: NodeId) -> bool {
        match self.doc.element(n) {
            Some(e) if e.foreign => matches!(e.name.as_str(), "foreignobject" | "desc" | "title" | "mi" | "mo" | "mn" | "ms" | "mtext" | "annotation-xml"),
            Some(e) => SPECIAL.contains(&e.name.as_str()),
            None => false,
        }
    }

    fn create(&mut self, tag: &Tag) -> NodeId {
        self.create_ns(tag, false)
    }

    fn create_ns(&mut self, tag: &Tag, foreign: bool) -> NodeId {
        if self.doc.nodes.len() >= MAX_NODES {
            self.full = true;
        }
        self.doc.new_node(NodeData::Element(Element { name: tag.name.clone(), attrs: tag.attrs.clone(), foreign }))
    }

    /// Where a new node goes: (parent, insert before this child).
    fn insertion_point(&self) -> (NodeId, Option<NodeId>) {
        let target = self.current();
        if self.foster && self.is_html(target) && ["table", "tbody", "tfoot", "thead", "tr"].contains(&self.doc.name(target)) {
            if let Some(ti) = self.open.iter().rposition(|&n| self.doc.is(n, "table")) {
                let table = self.open[ti];
                if let Some(p) = self.doc.parent(table) {
                    return (p, Some(table));
                }
                if ti > 0 {
                    return (self.open[ti - 1], None);
                }
            }
        }
        (target, None)
    }

    fn insert_node(&mut self, node: NodeId) {
        let (parent, before) = self.insertion_point();
        match before {
            Some(b) => self.doc.insert_before(parent, node, b),
            None => self.doc.append(parent, node),
        }
    }

    /// Inserts an element for `tag` and pushes it, unless the tree is
    /// already MAX_DEPTH deep: then it becomes a leaf. Returns (node, pushed).
    fn insert(&mut self, tag: &Tag) -> (NodeId, bool) {
        let n = self.create(tag);
        self.insert_node(n);
        if self.open.len() < MAX_DEPTH || super::tokenizer::raw_kind(&tag.name).is_some() {
            self.open.push(n);
            (n, true)
        } else {
            (n, false)
        }
    }

    fn insert_void(&mut self, tag: &Tag) -> NodeId {
        let n = self.create(tag);
        self.insert_node(n);
        n
    }

    fn insert_text(&mut self, s: &str) {
        if s.is_empty() || self.full {
            return;
        }
        let (parent, before) = self.insertion_point();
        if parent == Document::ROOT {
            return;
        }
        let prev = match before {
            Some(b) => {
                let ch = self.doc.children(parent);
                ch.iter().position(|&c| c == b).and_then(|i| i.checked_sub(1)).map(|i| ch[i])
            }
            None => self.doc.children(parent).last().copied(),
        };
        if let Some(p) = prev {
            if let NodeData::Text(t) = &mut self.doc.nodes[p].data {
                t.push_str(s);
                return;
            }
        }
        if self.doc.nodes.len() >= MAX_NODES {
            self.full = true;
            return;
        }
        let n = self.doc.new_node(NodeData::Text(s.to_string()));
        match before {
            Some(b) => self.doc.insert_before(parent, n, b),
            None => self.doc.append(parent, n),
        }
    }

    fn insert_comment(&mut self, s: &str, parent: Option<NodeId>) {
        if self.doc.nodes.len() >= MAX_NODES {
            return;
        }
        let n = self.doc.new_node(NodeData::Comment(s.to_string()));
        match parent {
            Some(p) => self.doc.append(p, n),
            None => self.insert_node(n),
        }
    }

    fn pop(&mut self) -> Option<NodeId> {
        self.open.pop()
    }

    fn pop_until(&mut self, name: &str) {
        while let Some(n) = self.open.pop() {
            if self.doc.is(n, name) {
                break;
            }
        }
    }

    fn pop_until_any(&mut self, names: &[&str]) {
        while let Some(n) = self.open.pop() {
            if self.is_html(n) && names.contains(&self.doc.name(n)) {
                break;
            }
        }
    }

    fn in_scope_with(&self, name: &str, extra: &[&str]) -> bool {
        for &n in self.open.iter().rev() {
            if self.doc.is(n, name) {
                return true;
            }
            let e = self.doc.name(n);
            if self.is_html(n) {
                if ["applet", "caption", "html", "table", "td", "th", "marquee", "object", "template"].contains(&e) || extra.contains(&e) {
                    return false;
                }
            } else if matches!(e, "foreignobject" | "desc" | "title" | "mi" | "mo" | "mn" | "ms" | "mtext" | "annotation-xml") {
                return false;
            }
        }
        false
    }

    fn in_scope(&self, name: &str) -> bool {
        self.in_scope_with(name, &[])
    }
    fn in_button_scope(&self, name: &str) -> bool {
        self.in_scope_with(name, &["button"])
    }
    fn in_list_scope(&self, name: &str) -> bool {
        self.in_scope_with(name, &["ol", "ul"])
    }
    fn in_table_scope(&self, name: &str) -> bool {
        for &n in self.open.iter().rev() {
            if self.doc.is(n, name) {
                return true;
            }
            if self.is_html(n) && ["html", "table", "template"].contains(&self.doc.name(n)) {
                return false;
            }
        }
        false
    }
    fn in_select_scope(&self, name: &str) -> bool {
        for &n in self.open.iter().rev() {
            if self.doc.is(n, name) {
                return true;
            }
            if !(self.is_html(n) && ["optgroup", "option"].contains(&self.doc.name(n))) {
                return false;
            }
        }
        false
    }

    fn generate_implied(&mut self, except: Option<&str>) {
        while let Some(&n) = self.open.last() {
            let name = self.doc.name(n);
            if self.is_html(n) && IMPLIED_END.contains(&name) && Some(name) != except {
                self.open.pop();
            } else {
                break;
            }
        }
    }

    fn close_p(&mut self) {
        if self.in_button_scope("p") {
            self.generate_implied(Some("p"));
            self.pop_until("p");
        }
    }

    fn clear_to_marker(&mut self) {
        while let Some(e) = self.afe.pop() {
            if matches!(e, Afe::Marker) {
                break;
            }
        }
    }

    fn afe_index(&self, node: NodeId) -> Option<usize> {
        self.afe.iter().position(|e| matches!(e, Afe::El(n, _) if *n == node))
    }

    fn push_afe(&mut self, node: NodeId, tag: &Tag) {
        // Noah's Ark: at most three identical entries after the last marker.
        let mut same = Vec::new();
        for (i, e) in self.afe.iter().enumerate().rev() {
            match e {
                Afe::Marker => break,
                Afe::El(_, t) if t.name == tag.name && t.attrs == tag.attrs => same.push(i),
                _ => {}
            }
        }
        if same.len() >= 3 {
            self.afe.remove(*same.last().unwrap());
        }
        if self.afe.len() >= MAX_AFE {
            // Bounded: forget the oldest formatting element.
            if let Some(i) = self.afe.iter().position(|e| matches!(e, Afe::El(..))) {
                self.afe.remove(i);
            }
        }
        self.afe.push(Afe::El(node, tag.clone()));
    }

    fn reconstruct_afe(&mut self) {
        let Some(last) = self.afe.last() else { return };
        match last {
            Afe::Marker => return,
            Afe::El(n, _) if self.open.contains(n) => return,
            _ => {}
        }
        let mut i = self.afe.len() - 1;
        while i > 0 {
            match &self.afe[i - 1] {
                Afe::Marker => break,
                Afe::El(n, _) if self.open.contains(n) => break,
                _ => i -= 1,
            }
        }
        for j in i..self.afe.len() {
            if self.open.len() >= MAX_DEPTH {
                break;
            }
            let Afe::El(_, tag) = self.afe[j].clone() else { continue };
            let (n, _) = self.insert(&tag);
            self.afe[j] = Afe::El(n, tag);
        }
    }

    fn reset_mode(&mut self) {
        for (i, &n) in self.open.iter().enumerate().rev() {
            let last = i == 0;
            let name = self.doc.name(n);
            if !self.is_html(n) {
                continue;
            }
            self.mode = match name {
                "select" => {
                    if self.open[..i].iter().any(|&a| self.doc.is(a, "table")) {
                        Mode::InSelectInTable
                    } else {
                        Mode::InSelect
                    }
                }
                "td" | "th" if !last => Mode::InCell,
                "tr" => Mode::InRow,
                "tbody" | "thead" | "tfoot" => Mode::InTableBody,
                "caption" => Mode::InCaption,
                "colgroup" => Mode::InColumnGroup,
                "table" => Mode::InTable,
                "head" if !last => Mode::InHead,
                "body" => Mode::InBody,
                "frameset" => Mode::InFrameset,
                "html" => {
                    if self.head.is_none() {
                        Mode::BeforeHead
                    } else {
                        Mode::AfterHead
                    }
                }
                _ if last => Mode::InBody,
                _ => continue,
            };
            return;
        }
        self.mode = Mode::InBody;
    }

    // ---- dispatch -------------------------------------------------------

    fn dispatch(&mut self, t: Token) {
        // Foreign content (svg, math): its own simple rules.
        if let Some(&cur) = self.open.last() {
            if self.is_foreign(cur) {
                match &t {
                    Token::StartTag(tag) if !BREAKOUT.contains(&tag.name.as_str()) && !(tag.name == "font" && tag.attrs.iter().any(|(k, _)| k == "color" || k == "face" || k == "size")) => {
                        let n = self.create_ns(tag, true);
                        self.insert_node(n);
                        if !tag.self_closing && self.open.len() < MAX_DEPTH {
                            self.open.push(n);
                        }
                        // Foreign <title>/<style>/<script> contents: raw text either way.
                        if tag.self_closing {
                            self.tok.set_raw(None);
                        }
                        return;
                    }
                    Token::StartTag(_) => {
                        // Break out to HTML.
                        while let Some(&n) = self.open.last() {
                            if self.is_foreign(n) {
                                self.open.pop();
                            } else {
                                break;
                            }
                        }
                    }
                    Token::EndTag(tag) => {
                        // Pop to the matching foreign element, if there is one.
                        let mut i = self.open.len();
                        while i > 0 && self.is_foreign(self.open[i - 1]) {
                            if self.doc.name(self.open[i - 1]).eq_ignore_ascii_case(&tag.name) {
                                self.open.truncate(i - 1);
                                return;
                            }
                            i -= 1;
                        }
                    }
                    Token::Text(s) => {
                        self.insert_text(s);
                        return;
                    }
                    Token::Comment(c) => {
                        self.insert_comment(c, None);
                        return;
                    }
                    _ => {}
                }
            }
        }
        self.process(t, self.mode);
    }

    fn process(&mut self, t: Token, mode: Mode) {
        match mode {
            Mode::Initial => match t {
                Token::Text(s) => {
                    let (_, rest) = split_ws(&s);
                    if !rest.is_empty() {
                        self.doc.quirks = true;
                        self.mode = Mode::BeforeHtml;
                        self.process(Token::Text(rest.to_string()), Mode::BeforeHtml);
                    }
                }
                Token::Comment(c) => self.insert_comment(&c, Some(Document::ROOT)),
                Token::Doctype { name, quirks } => {
                    let n = self.doc.new_node(NodeData::Doctype(name));
                    self.doc.append(Document::ROOT, n);
                    self.doc.quirks = quirks;
                    self.mode = Mode::BeforeHtml;
                }
                t => {
                    self.doc.quirks = true;
                    self.mode = Mode::BeforeHtml;
                    self.process(t, Mode::BeforeHtml);
                }
            },
            Mode::BeforeHtml => match t {
                Token::Doctype { .. } => {}
                Token::Comment(c) => self.insert_comment(&c, Some(Document::ROOT)),
                Token::Text(s) if is_ws_text(&s) => {}
                Token::Text(s) if s.starts_with(|c: char| c.is_ascii_whitespace()) => {
                    let (_, rest) = split_ws(&s);
                    self.process(Token::Text(rest.to_string()), mode);
                }
                Token::StartTag(tag) if tag.name == "html" => {
                    let n = self.create(&tag);
                    self.doc.append(Document::ROOT, n);
                    self.open.push(n);
                    self.mode = Mode::BeforeHead;
                }
                Token::EndTag(tag) if !["head", "body", "html", "br"].contains(&tag.name.as_str()) => {}
                t => {
                    let n = self.create(&start("html"));
                    self.doc.append(Document::ROOT, n);
                    self.open.push(n);
                    self.mode = Mode::BeforeHead;
                    self.process(t, Mode::BeforeHead);
                }
            },
            Mode::BeforeHead => match t {
                Token::Text(s) if is_ws_text(&s) => {}
                Token::Text(s) if s.starts_with(|c: char| c.is_ascii_whitespace()) => {
                    let (_, rest) = split_ws(&s);
                    self.process(Token::Text(rest.to_string()), mode);
                }
                Token::Comment(c) => self.insert_comment(&c, None),
                Token::Doctype { .. } => {}
                Token::StartTag(tag) if tag.name == "html" => self.in_body(Token::StartTag(tag)),
                Token::StartTag(tag) if tag.name == "head" => {
                    let (n, _) = self.insert(&tag);
                    self.head = Some(n);
                    self.mode = Mode::InHead;
                }
                Token::EndTag(tag) if !["head", "body", "html", "br"].contains(&tag.name.as_str()) => {}
                t => {
                    let (n, _) = self.insert(&start("head"));
                    self.head = Some(n);
                    self.mode = Mode::InHead;
                    self.process(t, Mode::InHead);
                }
            },
            Mode::InHead => self.in_head(t),
            Mode::InHeadNoscript => match t {
                Token::EndTag(tag) if tag.name == "noscript" => {
                    self.pop();
                    self.mode = Mode::InHead;
                }
                Token::Text(ref s) if is_ws_text(s) => self.in_head(t),
                Token::Comment(_) => self.in_head(t),
                Token::StartTag(ref tag) if ["basefont", "bgsound", "link", "meta", "noframes", "style"].contains(&tag.name.as_str()) => self.in_head(t),
                Token::StartTag(ref tag) if tag.name == "head" || tag.name == "noscript" => {}
                Token::EndTag(ref tag) if tag.name != "br" => {}
                t => {
                    self.pop();
                    self.mode = Mode::InHead;
                    self.process(t, Mode::InHead);
                }
            },
            Mode::AfterHead => match t {
                Token::Text(s) => {
                    let (ws, rest) = split_ws(&s);
                    self.insert_text(ws);
                    if !rest.is_empty() {
                        self.implied_body(Token::Text(rest.to_string()));
                    }
                }
                Token::Comment(c) => self.insert_comment(&c, None),
                Token::Doctype { .. } => {}
                Token::StartTag(tag) if tag.name == "html" => self.in_body(Token::StartTag(tag)),
                Token::StartTag(tag) if tag.name == "body" => {
                    self.insert(&tag);
                    self.mode = Mode::InBody;
                }
                Token::StartTag(tag) if tag.name == "frameset" => {
                    self.insert(&tag);
                    self.mode = Mode::InFrameset;
                }
                Token::StartTag(tag)
                    if ["base", "basefont", "bgsound", "link", "meta", "noframes", "script", "style", "template", "title"].contains(&tag.name.as_str()) =>
                {
                    if let Some(h) = self.head {
                        self.open.push(h);
                        self.in_head(Token::StartTag(tag));
                        if let Some(i) = self.open.iter().rposition(|&n| n == h) {
                            self.open.remove(i);
                        }
                    }
                }
                Token::StartTag(tag) if tag.name == "head" => {}
                Token::EndTag(tag) if !["body", "html", "br"].contains(&tag.name.as_str()) => {}
                t => self.implied_body(t),
            },
            Mode::InBody => self.in_body(t),
            Mode::Text => match t {
                Token::Text(s) => self.insert_text(&s),
                Token::Eof => {
                    self.pop();
                    self.mode = self.orig_mode;
                    self.process(Token::Eof, self.orig_mode);
                }
                Token::EndTag(_) => {
                    self.pop();
                    self.mode = self.orig_mode;
                }
                _ => {}
            },
            Mode::InTable => self.in_table(t),
            Mode::InCaption => self.in_caption(t),
            Mode::InColumnGroup => match t {
                Token::Text(s) => {
                    let (ws, rest) = split_ws(&s);
                    self.insert_text(ws);
                    if !rest.is_empty() {
                        self.colgroup_else(Token::Text(rest.to_string()));
                    }
                }
                Token::Comment(c) => self.insert_comment(&c, None),
                Token::StartTag(tag) if tag.name == "col" => {
                    self.insert_void(&tag);
                }
                Token::EndTag(tag) if tag.name == "colgroup" => {
                    if self.cur_is("colgroup") {
                        self.pop();
                        self.mode = Mode::InTable;
                    }
                }
                Token::EndTag(tag) if tag.name == "col" => {}
                Token::StartTag(tag) if tag.name == "template" => self.in_head(Token::StartTag(tag)),
                Token::Eof => self.in_body(Token::Eof),
                t => self.colgroup_else(t),
            },
            Mode::InTableBody => self.in_table_body(t),
            Mode::InRow => self.in_row(t),
            Mode::InCell => self.in_cell(t),
            Mode::InSelect | Mode::InSelectInTable => self.in_select(t, mode == Mode::InSelectInTable),
            Mode::AfterBody => match t {
                Token::Text(ref s) if is_ws_text(s) => self.in_body(t),
                Token::Comment(c) => {
                    let html = self.open.first().copied();
                    self.insert_comment(&c, html);
                }
                Token::Doctype { .. } => {}
                Token::StartTag(ref tag) if tag.name == "html" => self.in_body(t),
                Token::EndTag(ref tag) if tag.name == "html" => self.mode = Mode::AfterAfterBody,
                Token::Eof => {}
                t => {
                    self.mode = Mode::InBody;
                    self.in_body(t);
                }
            },
            Mode::InFrameset | Mode::AfterFrameset => match t {
                Token::Text(s) => {
                    let ws: String = s.chars().filter(|c| c.is_ascii_whitespace()).collect();
                    self.insert_text(&ws);
                }
                Token::Comment(c) => self.insert_comment(&c, None),
                Token::StartTag(tag) if tag.name == "frameset" && mode == Mode::InFrameset => {
                    self.insert(&tag);
                }
                Token::EndTag(tag) if tag.name == "frameset" && mode == Mode::InFrameset => {
                    if !self.cur_is("html") {
                        self.pop();
                    }
                    if !self.cur_is("frameset") {
                        self.mode = Mode::AfterFrameset;
                    }
                }
                Token::StartTag(tag) if tag.name == "frame" && mode == Mode::InFrameset => {
                    self.insert_void(&tag);
                }
                Token::StartTag(tag) if tag.name == "noframes" => self.in_head(Token::StartTag(tag)),
                Token::EndTag(tag) if tag.name == "html" && mode == Mode::AfterFrameset => self.mode = Mode::AfterAfterBody,
                _ => {}
            },
            Mode::AfterAfterBody => match t {
                Token::Comment(c) => self.insert_comment(&c, Some(Document::ROOT)),
                Token::Doctype { .. } | Token::Eof => {}
                Token::Text(ref s) if is_ws_text(s) => self.in_body(t),
                Token::StartTag(ref tag) if tag.name == "html" => self.in_body(t),
                t => {
                    self.mode = Mode::InBody;
                    self.in_body(t);
                }
            },
        }
    }

    fn implied_body(&mut self, t: Token) {
        self.insert(&start("body"));
        self.mode = Mode::InBody;
        self.process(t, Mode::InBody);
    }

    fn colgroup_else(&mut self, t: Token) {
        if self.cur_is("colgroup") {
            self.pop();
            self.mode = Mode::InTable;
            self.process(t, Mode::InTable);
        }
    }

    fn raw_element(&mut self, tag: &Tag) {
        self.insert(tag);
        self.orig_mode = if self.mode == Mode::Text { self.orig_mode } else { self.mode };
        self.mode = Mode::Text;
    }

    fn in_head(&mut self, t: Token) {
        match t {
            Token::Text(s) => {
                let (ws, rest) = split_ws(&s);
                self.insert_text(ws);
                if !rest.is_empty() {
                    self.head_else(Token::Text(rest.to_string()));
                }
            }
            Token::Comment(c) => self.insert_comment(&c, None),
            Token::Doctype { .. } => {}
            Token::StartTag(tag) => match tag.name.as_str() {
                "html" => self.in_body(Token::StartTag(tag)),
                "base" | "basefont" | "bgsound" | "link" | "meta" => {
                    self.insert_void(&tag);
                }
                "title" | "noframes" | "style" | "script" => self.raw_element(&tag),
                "noscript" => {
                    self.insert(&tag);
                    self.mode = Mode::InHeadNoscript;
                }
                "template" => {
                    self.insert(&tag);
                    self.afe.push(Afe::Marker);
                }
                "head" => {}
                _ => self.head_else(Token::StartTag(tag)),
            },
            Token::EndTag(tag) => match tag.name.as_str() {
                "head" => {
                    self.pop();
                    self.mode = Mode::AfterHead;
                }
                "template" => {
                    if self.open.iter().any(|&n| self.doc.is(n, "template")) {
                        self.generate_implied(None);
                        self.pop_until("template");
                        self.clear_to_marker();
                        self.reset_mode();
                    }
                }
                "body" | "html" | "br" => self.head_else(Token::EndTag(tag)),
                _ => {}
            },
            Token::Eof => self.head_else(Token::Eof),
        }
    }

    fn head_else(&mut self, t: Token) {
        if self.cur_is("head") {
            self.pop();
        }
        self.mode = Mode::AfterHead;
        self.process(t, Mode::AfterHead);
    }

    fn in_body(&mut self, t: Token) {
        match t {
            Token::Text(s) => {
                self.reconstruct_afe();
                self.insert_text(&s);
            }
            Token::Comment(c) => self.insert_comment(&c, None),
            Token::Doctype { .. } => {}
            Token::StartTag(tag) => self.body_start(tag),
            Token::EndTag(tag) => self.body_end(tag),
            Token::Eof => {}
        }
    }

    fn body_start(&mut self, mut tag: Tag) {
        let name = tag.name.clone();
        let n = name.as_str();
        match n {
            "html" => {
                if let Some(&html) = self.open.first() {
                    if let Some(e) = self.doc.element_mut(html) {
                        for (k, v) in tag.attrs {
                            if !e.attrs.iter().any(|(a, _)| *a == k) {
                                e.attrs.push((k, v));
                            }
                        }
                    }
                }
            }
            "base" | "basefont" | "bgsound" | "link" | "meta" | "noframes" | "script" | "style" | "template" | "title" => {
                self.in_head(Token::StartTag(tag))
            }
            "body" => {
                if let Some(&body) = self.open.get(1) {
                    if self.doc.is(body, "body") {
                        if let Some(e) = self.doc.element_mut(body) {
                            for (k, v) in tag.attrs {
                                if !e.attrs.iter().any(|(a, _)| *a == k) {
                                    e.attrs.push((k, v));
                                }
                            }
                        }
                    }
                }
            }
            "frameset" => {}
            _ if BLOCKISH.contains(&n) => {
                self.close_p();
                self.insert(&tag);
            }
            _ if HEADINGS.contains(&n) => {
                self.close_p();
                if self.cur_in(HEADINGS) {
                    self.pop();
                }
                self.insert(&tag);
            }
            "pre" | "listing" => {
                self.close_p();
                self.insert(&tag);
                self.skip_lf = true;
            }
            "form" => {
                if self.form.is_some() {
                    return;
                }
                self.close_p();
                let (f, _) = self.insert(&tag);
                self.form = Some(f);
            }
            "li" | "dd" | "dt" => {
                let closers: &[&str] = if n == "li" { &["li"] } else { &["dd", "dt"] };
                for i in (0..self.open.len()).rev() {
                    let node = self.open[i];
                    let nn = self.doc.name(node).to_string();
                    if self.is_html(node) && closers.contains(&nn.as_str()) {
                        self.generate_implied(Some(&nn));
                        self.pop_until(&nn);
                        break;
                    }
                    if self.is_special(node) && !["address", "div", "p"].contains(&nn.as_str()) {
                        break;
                    }
                }
                self.close_p();
                self.insert(&tag);
            }
            "plaintext" => {
                self.close_p();
                self.insert(&tag);
            }
            "button" => {
                if self.in_scope("button") {
                    self.generate_implied(None);
                    self.pop_until("button");
                }
                self.reconstruct_afe();
                self.insert(&tag);
            }
            "a" => {
                let active_a = self.afe.iter().rev().take_while(|e| !matches!(e, Afe::Marker)).find_map(|e| match e {
                    Afe::El(node, t) if t.name == "a" => Some(*node),
                    _ => None,
                });
                if let Some(old) = active_a {
                    self.adoption("a");
                    if let Some(i) = self.afe_index(old) {
                        self.afe.remove(i);
                    }
                    if let Some(i) = self.open.iter().position(|&x| x == old) {
                        self.open.remove(i);
                    }
                }
                self.reconstruct_afe();
                let (node, pushed) = self.insert(&tag);
                if pushed {
                    self.push_afe(node, &tag);
                }
            }
            "nobr" => {
                self.reconstruct_afe();
                if self.in_scope("nobr") {
                    self.adoption("nobr");
                    self.reconstruct_afe();
                }
                let (node, pushed) = self.insert(&tag);
                if pushed {
                    self.push_afe(node, &tag);
                }
            }
            _ if FORMATTING.contains(&n) => {
                self.reconstruct_afe();
                let (node, pushed) = self.insert(&tag);
                if pushed {
                    self.push_afe(node, &tag);
                }
            }
            "applet" | "marquee" | "object" => {
                self.reconstruct_afe();
                let (_, pushed) = self.insert(&tag);
                if pushed {
                    self.afe.push(Afe::Marker);
                }
            }
            "table" => {
                if !self.doc.quirks {
                    self.close_p();
                }
                let (_, pushed) = self.insert(&tag);
                if pushed {
                    self.mode = Mode::InTable;
                }
            }
            "area" | "br" | "embed" | "img" | "keygen" | "wbr" | "input" => {
                self.reconstruct_afe();
                self.insert_void(&tag);
            }
            "param" | "source" | "track" => {
                self.insert_void(&tag);
            }
            "hr" => {
                self.close_p();
                self.insert_void(&tag);
            }
            "image" => {
                tag.name = "img".into();
                self.body_start(tag);
            }
            "textarea" => {
                self.insert(&tag);
                self.skip_lf = true;
                self.orig_mode = self.mode;
                self.mode = Mode::Text;
            }
            "xmp" => {
                self.close_p();
                self.reconstruct_afe();
                self.raw_element(&tag);
            }
            "iframe" | "noembed" => self.raw_element(&tag),
            "select" => {
                self.reconstruct_afe();
                let (_, pushed) = self.insert(&tag);
                if pushed {
                    self.mode = if matches!(self.mode, Mode::InTable | Mode::InCaption | Mode::InTableBody | Mode::InRow | Mode::InCell) {
                        Mode::InSelectInTable
                    } else {
                        Mode::InSelect
                    };
                }
            }
            "optgroup" | "option" => {
                if self.cur_is("option") {
                    self.pop();
                }
                self.reconstruct_afe();
                self.insert(&tag);
            }
            "math" | "svg" => {
                self.reconstruct_afe();
                let node = self.create_ns(&tag, true);
                self.insert_node(node);
                if !tag.self_closing && self.open.len() < MAX_DEPTH {
                    self.open.push(node);
                }
            }
            "caption" | "col" | "colgroup" | "frame" | "head" | "tbody" | "td" | "tfoot" | "th" | "thead" | "tr" => {}
            _ => {
                self.reconstruct_afe();
                self.insert(&tag);
            }
        }
    }

    fn body_end(&mut self, tag: Tag) {
        let n = tag.name.as_str();
        match n {
            "body" | "html" => {
                if self.in_scope("body") {
                    self.mode = Mode::AfterBody;
                }
            }
            "address" | "article" | "aside" | "blockquote" | "button" | "center" | "details" | "dialog" | "dir" | "div" | "dl"
            | "fieldset" | "figcaption" | "figure" | "footer" | "header" | "hgroup" | "listing" | "main" | "menu" | "nav" | "ol"
            | "pre" | "search" | "section" | "summary" | "ul" => {
                if self.in_scope(n) {
                    self.generate_implied(None);
                    self.pop_until(n);
                }
            }
            "form" => {
                let node = self.form.take();
                if let Some(node) = node {
                    if self.open.contains(&node) && self.in_scope("form") {
                        self.generate_implied(None);
                        if let Some(i) = self.open.iter().position(|&x| x == node) {
                            self.open.remove(i);
                        }
                    }
                }
            }
            "p" => {
                if !self.in_button_scope("p") {
                    self.insert(&start("p"));
                }
                self.close_p();
            }
            "li" => {
                if self.in_list_scope("li") {
                    self.generate_implied(Some("li"));
                    self.pop_until("li");
                }
            }
            "dd" | "dt" => {
                if self.in_scope(n) {
                    self.generate_implied(Some(n));
                    self.pop_until(n);
                }
            }
            _ if HEADINGS.contains(&n) => {
                if HEADINGS.iter().any(|h| self.in_scope(h)) {
                    self.generate_implied(None);
                    self.pop_until_any(HEADINGS);
                }
            }
            _ if FORMATTING.contains(&n) => self.adoption(n),
            "applet" | "marquee" | "object" => {
                if self.in_scope(n) {
                    self.generate_implied(None);
                    self.pop_until(n);
                    self.clear_to_marker();
                }
            }
            "br" => self.body_start(start("br")),
            "template" => self.in_head(Token::EndTag(tag)),
            _ => self.any_other_end(n),
        }
    }

    fn any_other_end(&mut self, name: &str) {
        for i in (0..self.open.len()).rev() {
            let node = self.open[i];
            if self.doc.is(node, name) {
                self.generate_implied(Some(name));
                self.open.truncate(i);
                return;
            }
            if self.is_special(node) {
                return;
            }
        }
    }

    /// The adoption agency algorithm for an end tag of formatting element `subject`.
    fn adoption(&mut self, subject: &str) {
        if let Some(&cur) = self.open.last() {
            if self.doc.is(cur, subject) && self.afe_index(cur).is_none() {
                self.open.pop();
                return;
            }
        }
        for _ in 0..8 {
            // The formatting element: the last `subject` after the last marker.
            let mut fe_i = None;
            for (i, e) in self.afe.iter().enumerate().rev() {
                match e {
                    Afe::Marker => break,
                    Afe::El(_, t) if t.name == subject => {
                        fe_i = Some(i);
                        break;
                    }
                    _ => {}
                }
            }
            let Some(fe_i) = fe_i else {
                self.any_other_end(subject);
                return;
            };
            let Afe::El(fe, fe_tag) = self.afe[fe_i].clone() else { return };
            let Some(fe_stack) = self.open.iter().position(|&n| n == fe) else {
                self.afe.remove(fe_i);
                return;
            };
            if !self.in_scope(subject) {
                return;
            }
            let fb_stack = (fe_stack + 1..self.open.len()).find(|&i| self.is_special(self.open[i]));
            let Some(fb_stack) = fb_stack else {
                self.open.truncate(fe_stack);
                self.afe.remove(fe_i);
                return;
            };
            let furthest = self.open[fb_stack];
            let common = self.open[fe_stack.saturating_sub(1)];
            let mut bookmark = fe_i;
            let mut node_i = fb_stack;
            let mut last = furthest;
            let mut counter = 0;
            loop {
                counter += 1;
                if node_i == 0 {
                    break;
                }
                node_i -= 1;
                let node = self.open[node_i];
                if node == fe {
                    break;
                }
                let mut ai = self.afe_index(node);
                if counter > 3 {
                    if let Some(i) = ai {
                        self.afe.remove(i);
                        if i < bookmark {
                            bookmark -= 1;
                        }
                        ai = None;
                    }
                }
                let Some(ai) = ai else {
                    self.open.remove(node_i);
                    continue;
                };
                let Afe::El(_, ntag) = self.afe[ai].clone() else { break };
                let newn = self.create(&ntag);
                self.afe[ai] = Afe::El(newn, ntag);
                self.open[node_i] = newn;
                if last == furthest {
                    bookmark = ai + 1;
                }
                self.doc.append(newn, last);
                last = newn;
            }
            // Insert `last` at the appropriate place for `common`.
            let foster_here = self.is_html(common) && ["table", "tbody", "tfoot", "thead", "tr"].contains(&self.doc.name(common));
            if foster_here {
                if let Some(ti) = self.open.iter().rposition(|&n| self.doc.is(n, "table")) {
                    let table = self.open[ti];
                    match self.doc.parent(table) {
                        Some(p) => self.doc.insert_before(p, last, table),
                        None => self.doc.append(common, last),
                    }
                } else {
                    self.doc.append(common, last);
                }
            } else {
                self.doc.append(common, last);
            }
            let newfe = self.create(&fe_tag);
            let kids: Vec<NodeId> = self.doc.children(furthest).to_vec();
            for k in kids {
                self.doc.append(newfe, k);
            }
            self.doc.append(furthest, newfe);
            if let Some(i) = self.afe_index(fe) {
                self.afe.remove(i);
                if i < bookmark {
                    bookmark -= 1;
                }
            }
            let bookmark = bookmark.min(self.afe.len());
            self.afe.insert(bookmark, Afe::El(newfe, fe_tag));
            if let Some(i) = self.open.iter().position(|&n| n == fe) {
                self.open.remove(i);
            }
            if let Some(i) = self.open.iter().position(|&n| n == furthest) {
                self.open.insert(i + 1, newfe);
            }
        }
    }

    // ---- tables ---------------------------------------------------------

    fn clear_to(&mut self, names: &[&str]) {
        while let Some(&n) = self.open.last() {
            if (self.is_html(n) && names.contains(&self.doc.name(n))) || self.open.len() <= 1 {
                break;
            }
            self.open.pop();
        }
    }

    fn in_table(&mut self, t: Token) {
        match t {
            Token::Text(s) => {
                if self.cur_in(&["table", "tbody", "tfoot", "thead", "tr"]) {
                    if is_ws_text(&s) {
                        self.insert_text(&s);
                    } else {
                        self.foster = true;
                        self.in_body(Token::Text(s));
                        self.foster = false;
                    }
                } else {
                    self.in_body(Token::Text(s));
                }
            }
            Token::Comment(c) => self.insert_comment(&c, None),
            Token::Doctype { .. } => {}
            Token::StartTag(tag) => match tag.name.as_str() {
                "caption" => {
                    self.clear_to(&["table", "template", "html"]);
                    self.afe.push(Afe::Marker);
                    self.insert(&tag);
                    self.mode = Mode::InCaption;
                }
                "colgroup" => {
                    self.clear_to(&["table", "template", "html"]);
                    self.insert(&tag);
                    self.mode = Mode::InColumnGroup;
                }
                "col" => {
                    self.clear_to(&["table", "template", "html"]);
                    self.insert(&start("colgroup"));
                    self.mode = Mode::InColumnGroup;
                    self.process(Token::StartTag(tag), Mode::InColumnGroup);
                }
                "tbody" | "tfoot" | "thead" => {
                    self.clear_to(&["table", "template", "html"]);
                    self.insert(&tag);
                    self.mode = Mode::InTableBody;
                }
                "td" | "th" | "tr" => {
                    self.clear_to(&["table", "template", "html"]);
                    self.insert(&start("tbody"));
                    self.mode = Mode::InTableBody;
                    self.process(Token::StartTag(tag), Mode::InTableBody);
                }
                "table" => {
                    if self.in_table_scope("table") {
                        self.pop_until("table");
                        self.reset_mode();
                        let m = self.mode;
                        self.process(Token::StartTag(tag), m);
                    }
                }
                "style" | "script" | "template" => self.in_head(Token::StartTag(tag)),
                "input" if tag.attr("type").is_some_and(|t| t.eq_ignore_ascii_case("hidden")) => {
                    self.insert_void(&tag);
                }
                "form" => {
                    if self.form.is_none() {
                        let f = self.insert_void(&tag);
                        self.form = Some(f);
                    }
                }
                _ => {
                    self.foster = true;
                    self.in_body(Token::StartTag(tag));
                    self.foster = false;
                }
            },
            Token::EndTag(tag) => match tag.name.as_str() {
                "table" => {
                    if self.in_table_scope("table") {
                        self.pop_until("table");
                        self.reset_mode();
                    }
                }
                "body" | "caption" | "col" | "colgroup" | "html" | "tbody" | "td" | "tfoot" | "th" | "thead" | "tr" => {}
                "template" => self.in_head(Token::EndTag(tag)),
                _ => {
                    self.foster = true;
                    self.in_body(Token::EndTag(tag));
                    self.foster = false;
                }
            },
            Token::Eof => {}
        }
    }

    fn in_caption(&mut self, t: Token) {
        let closes = match &t {
            Token::EndTag(tag) if tag.name == "caption" => Some(false),
            Token::StartTag(tag) if ["caption", "col", "colgroup", "tbody", "td", "tfoot", "th", "thead", "tr"].contains(&tag.name.as_str()) => Some(true),
            Token::EndTag(tag) if tag.name == "table" => Some(true),
            Token::EndTag(tag) if ["body", "col", "colgroup", "html", "tbody", "td", "tfoot", "th", "thead", "tr"].contains(&tag.name.as_str()) => return,
            _ => None,
        };
        match closes {
            Some(reprocess) => {
                if !self.in_table_scope("caption") {
                    return;
                }
                self.generate_implied(None);
                self.pop_until("caption");
                self.clear_to_marker();
                self.mode = Mode::InTable;
                if reprocess {
                    self.process(t, Mode::InTable);
                }
            }
            None => self.in_body(t),
        }
    }

    fn in_table_body(&mut self, t: Token) {
        match &t {
            Token::StartTag(tag) if tag.name == "tr" => {
                self.clear_to(&["tbody", "tfoot", "thead", "template", "html"]);
                self.insert(tag);
                self.mode = Mode::InRow;
            }
            Token::StartTag(tag) if tag.name == "th" || tag.name == "td" => {
                self.clear_to(&["tbody", "tfoot", "thead", "template", "html"]);
                self.insert(&start("tr"));
                self.mode = Mode::InRow;
                self.process(t, Mode::InRow);
            }
            Token::EndTag(tag) if ["tbody", "tfoot", "thead"].contains(&tag.name.as_str()) => {
                if self.in_table_scope(&tag.name) {
                    self.clear_to(&["tbody", "tfoot", "thead", "template", "html"]);
                    self.pop();
                    self.mode = Mode::InTable;
                }
            }
            Token::StartTag(tag) if ["caption", "col", "colgroup", "tbody", "tfoot", "thead"].contains(&tag.name.as_str()) => self.table_body_exit(t),
            Token::EndTag(tag) if tag.name == "table" => self.table_body_exit(t),
            Token::EndTag(tag) if ["body", "caption", "col", "colgroup", "html", "td", "th", "tr"].contains(&tag.name.as_str()) => {}
            _ => self.in_table(t),
        }
    }

    fn table_body_exit(&mut self, t: Token) {
        if self.in_table_scope("tbody") || self.in_table_scope("thead") || self.in_table_scope("tfoot") {
            self.clear_to(&["tbody", "tfoot", "thead", "template", "html"]);
            self.pop();
            self.mode = Mode::InTable;
            self.process(t, Mode::InTable);
        }
    }

    fn end_row(&mut self) -> bool {
        if !self.in_table_scope("tr") {
            return false;
        }
        self.clear_to(&["tr", "template", "html"]);
        self.pop();
        self.mode = Mode::InTableBody;
        true
    }

    fn in_row(&mut self, t: Token) {
        match &t {
            Token::StartTag(tag) if tag.name == "th" || tag.name == "td" => {
                self.clear_to(&["tr", "template", "html"]);
                self.insert(tag);
                self.mode = Mode::InCell;
                self.afe.push(Afe::Marker);
            }
            Token::EndTag(tag) if tag.name == "tr" => {
                self.end_row();
            }
            Token::StartTag(tag) if ["caption", "col", "colgroup", "tbody", "tfoot", "thead", "tr"].contains(&tag.name.as_str()) => {
                if self.end_row() {
                    self.process(t, Mode::InTableBody);
                }
            }
            Token::EndTag(tag) if tag.name == "table" => {
                if self.end_row() {
                    self.process(t, Mode::InTableBody);
                }
            }
            Token::EndTag(tag) if ["tbody", "tfoot", "thead"].contains(&tag.name.as_str()) => {
                if self.in_table_scope(&tag.name) && self.end_row() {
                    self.process(t, Mode::InTableBody);
                }
            }
            Token::EndTag(tag) if ["body", "caption", "col", "colgroup", "html", "td", "th"].contains(&tag.name.as_str()) => {}
            _ => self.in_table(t),
        }
    }

    fn close_cell(&mut self) {
        self.generate_implied(None);
        self.pop_until_any(&["td", "th"]);
        self.clear_to_marker();
        self.mode = Mode::InRow;
    }

    fn in_cell(&mut self, t: Token) {
        match &t {
            Token::EndTag(tag) if tag.name == "td" || tag.name == "th" => {
                if self.in_table_scope(&tag.name) {
                    self.generate_implied(None);
                    self.pop_until(&tag.name.clone());
                    self.clear_to_marker();
                    self.mode = Mode::InRow;
                }
            }
            Token::StartTag(tag) if ["caption", "col", "colgroup", "tbody", "td", "tfoot", "th", "thead", "tr"].contains(&tag.name.as_str()) => {
                if self.in_table_scope("td") || self.in_table_scope("th") {
                    self.close_cell();
                    self.process(t, Mode::InRow);
                }
            }
            Token::EndTag(tag) if ["body", "caption", "col", "colgroup", "html"].contains(&tag.name.as_str()) => {}
            Token::EndTag(tag) if ["table", "tbody", "tfoot", "thead", "tr"].contains(&tag.name.as_str()) => {
                if self.in_table_scope(&tag.name) {
                    self.close_cell();
                    self.process(t, Mode::InRow);
                }
            }
            _ => self.in_body(t),
        }
    }

    fn in_select(&mut self, t: Token, in_table: bool) {
        match t {
            Token::Text(s) => self.insert_text(&s),
            Token::Comment(c) => self.insert_comment(&c, None),
            Token::StartTag(tag) => match tag.name.as_str() {
                "html" => self.in_body(Token::StartTag(tag)),
                "option" => {
                    if self.cur_is("option") {
                        self.pop();
                    }
                    self.insert(&tag);
                }
                "optgroup" => {
                    if self.cur_is("option") {
                        self.pop();
                    }
                    if self.cur_is("optgroup") {
                        self.pop();
                    }
                    self.insert(&tag);
                }
                "hr" => {
                    if self.cur_is("option") {
                        self.pop();
                    }
                    if self.cur_is("optgroup") {
                        self.pop();
                    }
                    self.insert_void(&tag);
                }
                "select" => {
                    if self.in_select_scope("select") {
                        self.pop_until("select");
                        self.reset_mode();
                    }
                }
                "input" | "keygen" | "textarea" => {
                    if self.in_select_scope("select") {
                        self.pop_until("select");
                        self.reset_mode();
                        let m = self.mode;
                        self.process(Token::StartTag(tag), m);
                    }
                }
                "caption" | "table" | "tbody" | "tfoot" | "thead" | "tr" | "td" | "th" if in_table => {
                    self.pop_until("select");
                    self.reset_mode();
                    let m = self.mode;
                    self.process(Token::StartTag(tag), m);
                }
                "script" | "template" => self.in_head(Token::StartTag(tag)),
                _ => {}
            },
            Token::EndTag(tag) => match tag.name.as_str() {
                "optgroup" => {
                    if self.cur_is("option") && self.open.len() >= 2 && self.doc.is(self.open[self.open.len() - 2], "optgroup") {
                        self.pop();
                    }
                    if self.cur_is("optgroup") {
                        self.pop();
                    }
                }
                "option" => {
                    if self.cur_is("option") {
                        self.pop();
                    }
                }
                "select" => {
                    if self.in_select_scope("select") {
                        self.pop_until("select");
                        self.reset_mode();
                    }
                }
                "caption" | "table" | "tbody" | "tfoot" | "thead" | "tr" | "td" | "th" if in_table => {
                    if self.in_table_scope(&tag.name) {
                        self.pop_until("select");
                        self.reset_mode();
                        let m = self.mode;
                        self.process(Token::EndTag(tag), m);
                    }
                }
                "template" => self.in_head(Token::EndTag(tag)),
                _ => {}
            },
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(html: &str) -> String {
        let d = parse(html);
        let b = d.body().expect("body");
        let s = d.outline(b);
        s.trim_start_matches("<body>").trim_end_matches("</body>").to_string()
    }

    #[test]
    fn implied_structure() {
        let d = parse("Hello");
        assert_eq!(d.outline(0), "<html><head></head><body>\"Hello\"</body></html>");
        let d = parse("<!doctype html><title>T</title><p>x");
        assert_eq!(d.outline(0), "<!html><html><head><title>\"T\"</title></head><body><p>\"x\"</p></body></html>");
        assert!(!d.quirks);
        assert!(parse("<p>x").quirks);
        assert_eq!(d.title(), "T");
    }

    #[test]
    fn paragraphs_and_lists_auto_close() {
        assert_eq!(body("<p>a<p>b<div>c</div>"), "<p>\"a\"</p><p>\"b\"</p><div>\"c\"</div>");
        assert_eq!(body("<ul><li>a<li>b</ul>"), "<ul><li>\"a\"</li><li>\"b\"</li></ul>");
        assert_eq!(body("<dl><dt>a<dd>b<dt>c</dl>"), "<dl><dt>\"a\"</dt><dd>\"b\"</dd><dt>\"c\"</dt></dl>");
        assert_eq!(body("<h1>a<h2>b</h2>"), "<h1>\"a\"</h1><h2>\"b\"</h2>");
        assert_eq!(body("<body></p>x"), "<p></p>\"x\"");
        assert_eq!(body("<p>a<ul><li>b</ul>"), "<p>\"a\"</p><ul><li>\"b\"</li></ul>");
        assert_eq!(body("<ul><li>a<ul><li>b</ul><li>c</ul>"), "<ul><li>\"a\"<ul><li>\"b\"</li></ul></li><li>\"c\"</li></ul>");
    }

    #[test]
    fn void_elements_and_end_br() {
        assert_eq!(body("a<br>b<img src=x>c<hr>d</br>e"), "\"a\"<br></br>\"b\"<img src=\"x\"></img>\"c\"<hr></hr>\"d\"<br></br>\"e\"");
    }

    #[test]
    fn formatting_misnesting() {
        assert_eq!(body("<b>1<i>2</b>3</i>"), "<b>\"1\"<i>\"2\"</i></b><i>\"3\"</i>");
        assert_eq!(body("<b>1<p>2</b>3</p>"), "<b>\"1\"</b><p><b>\"2\"</b>\"3\"</p>");
        assert_eq!(body("<p><b>x</p><p>y"), "<p><b>\"x\"</b></p><p><b>\"y\"</b></p>");
        assert_eq!(body("<a href=1>x<a href=2>y"), "<a href=\"1\">\"x\"</a><a href=\"2\">\"y\"</a>");
    }

    #[test]
    fn tables() {
        assert_eq!(
            body("<table><tr><td>a<td>b<tr><td>c</table>"),
            "<table><tbody><tr><td>\"a\"</td><td>\"b\"</td></tr><tr><td>\"c\"</td></tr></tbody></table>"
        );
        // Text in a table is fostered before it.
        assert_eq!(body("<table>x<tr><td>y</table>"), "\"x\"<table><tbody><tr><td>\"y\"</td></tr></tbody></table>");
        assert_eq!(
            body("<table><thead><tr><th>h<tbody><tr><td>d</table>"),
            "<table><thead><tr><th>\"h\"</th></tr></thead><tbody><tr><td>\"d\"</td></tr></tbody></table>"
        );
        assert_eq!(body("<table><td><p>a<td>b</table>"), "<table><tbody><tr><td><p>\"a\"</p></td><td>\"b\"</td></tr></tbody></table>");
        assert_eq!(body("<table><caption>c</caption><tr><td>1</table>"), "<table><caption>\"c\"</caption><tbody><tr><td>\"1\"</td></tr></tbody></table>");
    }

    #[test]
    fn head_content_and_raw_text() {
        let d = parse("<head><style>p{color:red}</style><script>var x = '<p>';</script><meta charset=utf-8></head><body>t");
        let head = d.find("head").unwrap();
        assert_eq!(d.outline(head), "<head><style>\"p{color:red}\"</style><script>\"var x = '<p>';\"</script><meta charset=\"utf-8\"></meta></head>");
        // A <style> after the body starts lands in the body.
        assert_eq!(body("<p>a<style>b{}</style>"), "<p>\"a\"<style>\"b{}\"</style></p>");
    }

    #[test]
    fn noscript_is_markup() {
        assert_eq!(body("<body><noscript><p>Enable JS</p></noscript>"), "<noscript><p>\"Enable JS\"</p></noscript>");
        let d = parse("<head><noscript><link rel=stylesheet href=a.css></noscript></head>");
        assert!(d.find("link").is_some());
    }

    #[test]
    fn select_and_options() {
        assert_eq!(body("<select><option>a<option>b</select>x"), "<select><option>\"a\"</option><option>\"b\"</option></select>\"x\"");
    }

    #[test]
    fn foreign_content() {
        assert_eq!(body("<svg><path/><title>t</title></svg><p>x"), "<svg><path></path><title>\"t\"</title></svg><p>\"x\"</p>");
        assert_eq!(body("<svg><g><p>out"), "<svg><g></g></svg><p>\"out\"</p>");
    }

    #[test]
    fn pre_drops_first_newline() {
        assert_eq!(body("<pre>\nline</pre>"), "<pre>\"line\"</pre>");
    }

    #[test]
    fn deep_nesting_is_capped() {
        let s = "<div>".repeat(100_000) + "x" + &"</div>".repeat(100_000);
        let d = parse(&s);
        assert!(d.depth() <= MAX_DEPTH + 5, "depth {}", d.depth());
        let s = "<b><i><u><s>".repeat(20_000) + "x";
        let d = parse(&s);
        assert!(d.depth() <= MAX_DEPTH + 5);
        let s = "<table><tr><td>".repeat(5_000);
        let d = parse(&s);
        assert!(d.depth() <= MAX_DEPTH + 5);
    }

    #[test]
    fn hostile_inputs_dont_panic() {
        let cases = [
            "<", "</", "<!", "<!-", "<!--", "&", "&#", "&#x", "<a", "<a b", "<a b=", "<a b='", "</a", "<table>",
            "<table><tr><td><table><tr><td>", "<select><table>", "<svg><svg><math>", "</table></td></tr>", "<frameset><frame>",
            "<p><table><p>", "<b><table><b><td></b>", "<a><table><a>", "<table><b><tr><td>x</b>y", "<template><td>",
            "<head></head><head>", "</html><p>", "<body><body x=1>", "<form><form>", "<option><optgroup>", "<nobr><nobr>",
            "<textarea>", "<title>", "<plaintext><b>", "<xmp></xmp>", "<!DOCTYPE>", "<!DOCTYPE html PUBLIC>", "\0\0<\0>",
            "<a><b><c><d><e></a></b></c></d></e>", "<b><p><i></b></p></i>", "<li><li><dd><dt><li>",
        ];
        for c in cases {
            let _ = parse(c);
            let _ = parse(&c.repeat(500));
        }
        // Pseudo-random soup of tag fragments.
        let parts = ["<p>", "</p>", "<b>", "</b>", "<table>", "<tr>", "<td>", "</td>", "</table>", "<a href=x>", "</a>", "text", "&amp", "<li>", "<svg>", "</svg>", "<select>", "<option>", "<!--", "-->", "<i>", "</div>", "<div>", "<caption>", "<col>", "<frameset>"];
        let mut seed = 12345u64;
        for _ in 0..50 {
            let mut s = String::new();
            for _ in 0..2000 {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                s.push_str(parts[(seed >> 33) as usize % parts.len()]);
            }
            let d = parse(&s);
            assert!(d.depth() <= MAX_DEPTH + 5);
        }
    }
}
