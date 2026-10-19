//! tokenizer.rs - HTML tokenization (WHATWG "13.2.5", condensed).
//!
//! Turns decoded text into tags, text, comments and doctypes. It follows the
//! spec's decisions where they change the result (character references and
//! their legacy no-';' forms, raw text and RCDATA elements, bogus comments,
//! duplicate attributes, tags cut off by the end of the file) and enforces
//! limits so hostile input costs at most linear time and bounded memory.

use super::entities;
use std::borrow::Cow;
use std::collections::VecDeque;

pub const MAX_ATTRS: usize = 64;
pub const MAX_ATTR_VALUE: usize = 64 * 1024;
pub const MAX_NAME: usize = 64;
pub const MAX_COMMENT: usize = 4096;

#[derive(Clone, Debug, PartialEq)]
pub struct Tag {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub self_closing: bool,
}

impl Tag {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Token {
    Doctype { name: String, quirks: bool },
    StartTag(Tag),
    EndTag(Tag),
    Comment(String),
    Text(String),
    Eof,
}

pub struct Tokenizer {
    src: String,
    pos: usize,
    /// Inside <script>, <style>, <title> ...: text up to "</name".
    raw: Option<(String, bool)>, // (name, decode character references)
    plaintext: bool,
    queue: VecDeque<Token>,
}

fn is_ws(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\x0C' | b'\r')
}

/// The elements whose contents are not markup.
pub fn raw_kind(name: &str) -> Option<bool> {
    match name {
        "script" | "style" | "xmp" | "iframe" | "noembed" | "noframes" => Some(false),
        "title" | "textarea" => Some(true),
        _ => None,
    }
}

impl Tokenizer {
    pub fn new(input: &str) -> Tokenizer {
        // Input stream preprocessing: newlines normalized, NULs replaced.
        let mut src = String::with_capacity(input.len());
        let mut chars = input.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\r' => {
                    if chars.peek() == Some(&'\n') {
                        chars.next();
                    }
                    src.push('\n');
                }
                '\0' => src.push('\u{FFFD}'),
                c => src.push(c),
            }
        }
        Tokenizer { src, pos: 0, raw: None, plaintext: false, queue: VecDeque::new() }
    }

    fn rest(&self) -> &str {
        &self.src[self.pos..]
    }

    fn starts_with_ci(&self, at: usize, pat: &str) -> bool {
        self.src.as_bytes().get(at..at + pat.len()).is_some_and(|b| b.eq_ignore_ascii_case(pat.as_bytes()))
    }

    /// Lets the tree builder leave raw text mode (e.g. after a foreign
    /// <title>, which is markup) or enter it.
    pub fn set_raw(&mut self, raw: Option<(String, bool)>) {
        self.raw = raw;
    }

    pub fn set_plaintext(&mut self) {
        self.plaintext = true;
    }

    pub fn next_token(&mut self) -> Token {
        if let Some(t) = self.queue.pop_front() {
            return t;
        }
        if self.pos >= self.src.len() {
            return Token::Eof;
        }
        if self.plaintext {
            let t = self.rest().to_string();
            self.pos = self.src.len();
            return Token::Text(t);
        }
        if let Some((name, rcdata)) = self.raw.take() {
            let end = self.find_raw_end(&name);
            let text = &self.src[self.pos..end];
            let t = if rcdata { decode_refs(text, false).into_owned() } else { text.to_string() };
            self.pos = end;
            if !t.is_empty() {
                return Token::Text(t);
            }
            if self.pos >= self.src.len() {
                return Token::Eof;
            }
        }
        let b = self.src.as_bytes();
        if b[self.pos] == b'<' {
            if let Some(t) = self.markup() {
                return t;
            }
            // A '<' that starts nothing is text.
            self.pos += 1;
            let mut t = String::from("<");
            t.push_str(&self.text_run());
            return Token::Text(t);
        }
        Token::Text(self.text_run())
    }

    /// Text up to the next '<', with character references decoded.
    fn text_run(&mut self) -> String {
        let end = self.rest().find('<').map_or(self.src.len(), |i| self.pos + i);
        let t = decode_refs(&self.src[self.pos..end], false).into_owned();
        self.pos = end;
        t
    }

    fn find_raw_end(&self, name: &str) -> usize {
        let b = self.src.as_bytes();
        let mut i = self.pos;
        while let Some(off) = self.src[i..].find("</") {
            let at = i + off;
            let after = at + 2 + name.len();
            if self.starts_with_ci(at + 2, name) && (after >= b.len() || is_ws(b[after]) || b[after] == b'/' || b[after] == b'>') {
                return at;
            }
            i = at + 2;
        }
        self.src.len()
    }

    /// Parses markup at '<'. None: the '<' is just text.
    fn markup(&mut self) -> Option<Token> {
        let b = self.src.as_bytes();
        let p = self.pos;
        let next = *b.get(p + 1)?;
        match next {
            b'!' => {
                if self.src[p..].starts_with("<!--") {
                    Some(self.comment())
                } else if self.starts_with_ci(p + 2, "doctype") {
                    Some(self.doctype())
                } else {
                    Some(self.bogus_comment(p + 2))
                }
            }
            b'/' => match b.get(p + 2) {
                Some(c) if c.is_ascii_alphabetic() => {
                    self.pos = p + 2;
                    match self.tag() {
                        Some(mut t) => {
                            t.attrs.clear();
                            Some(Token::EndTag(t))
                        }
                        None => Some(Token::Eof),
                    }
                }
                Some(b'>') => {
                    self.pos = p + 3;
                    Some(self.next_token())
                }
                None => None,
                _ => Some(self.bogus_comment(p + 2)),
            },
            b'?' => Some(self.bogus_comment(p + 1)),
            c if c.is_ascii_alphabetic() => {
                self.pos = p + 1;
                match self.tag() {
                    Some(t) => {
                        if t.name == "plaintext" {
                            self.plaintext = true;
                        } else if let Some(rc) = raw_kind(&t.name) {
                            self.raw = Some((t.name.clone(), rc));
                        }
                        Some(Token::StartTag(t))
                    }
                    // A tag cut off by the end of the file is dropped.
                    None => Some(Token::Eof),
                }
            }
            _ => None,
        }
    }

    fn comment(&mut self) -> Token {
        let start = self.pos + 4;
        let s = &self.src[start..];
        // "<!-->" and "<!--->" are (empty) comments.
        if s.starts_with('>') {
            self.pos = start + 1;
            return Token::Comment(String::new());
        }
        if s.starts_with("->") {
            self.pos = start + 2;
            return Token::Comment(String::new());
        }
        let (body, end) = match s.find("-->") {
            Some(i) => (&s[..i], start + i + 3),
            None => match s.find("--!>") {
                Some(i) => (&s[..i], start + i + 4),
                None => (s, self.src.len()),
            },
        };
        let t = truncate(body, MAX_COMMENT).to_string();
        self.pos = end;
        Token::Comment(t)
    }

    fn bogus_comment(&mut self, start: usize) -> Token {
        let s = &self.src[start..];
        let (body, end) = match s.find('>') {
            Some(i) => (&s[..i], start + i + 1),
            None => (s, self.src.len()),
        };
        let t = truncate(body, MAX_COMMENT).to_string();
        self.pos = end;
        Token::Comment(t)
    }

    fn doctype(&mut self) -> Token {
        let start = self.pos + 9;
        let s = &self.src[start.min(self.src.len())..];
        let (body, end) = match s.find('>') {
            Some(i) => (&s[..i], start + i + 1),
            None => (s, self.src.len()),
        };
        self.pos = end.min(self.src.len());
        let body = truncate(body, 1024);
        let name = body.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
        let lower = body.to_ascii_lowercase();
        let quirks = name != "html"
            || lower.contains("html 3")
            || lower.contains("html 2")
            || (lower.contains("html 4.01 transitional") && !lower.contains("http://"))
            || (lower.contains("html 4.01 frameset") && !lower.contains("http://"))
            || lower.contains("html 4.0 transitional");
        Token::Doctype { name, quirks }
    }

    /// Parses a tag name and attributes after "<" or "</". None at EOF.
    fn tag(&mut self) -> Option<Tag> {
        let b = self.src.as_bytes();
        let n = b.len();
        let mut i = self.pos;
        let start = i;
        while i < n && !is_ws(b[i]) && b[i] != b'/' && b[i] != b'>' {
            i += 1;
        }
        let name = lower_trunc(&self.src[start..i], MAX_NAME);
        let mut tag = Tag { name, attrs: Vec::new(), self_closing: false };
        loop {
            while i < n && is_ws(b[i]) {
                i += 1;
            }
            if i >= n {
                self.pos = n;
                return None;
            }
            match b[i] {
                b'>' => {
                    self.pos = i + 1;
                    return Some(tag);
                }
                b'/' => {
                    i += 1;
                    if i < n && b[i] == b'>' {
                        tag.self_closing = true;
                        self.pos = i + 1;
                        return Some(tag);
                    }
                    continue;
                }
                _ => {}
            }
            // Attribute name ('=' may start one, as in the spec).
            let ns = i;
            i += 1;
            while i < n && !is_ws(b[i]) && b[i] != b'/' && b[i] != b'>' && b[i] != b'=' {
                i += 1;
            }
            let aname = lower_trunc(&self.src[ns..i], MAX_NAME);
            while i < n && is_ws(b[i]) {
                i += 1;
            }
            let mut value = String::new();
            if i < n && b[i] == b'=' {
                i += 1;
                while i < n && is_ws(b[i]) {
                    i += 1;
                }
                if i >= n {
                    self.pos = n;
                    return None;
                }
                let (raw, next) = match b[i] {
                    q @ (b'"' | b'\'') => match self.src[i + 1..].find(q as char) {
                        Some(j) => (&self.src[i + 1..i + 1 + j], i + 2 + j),
                        None => {
                            self.pos = n;
                            return None;
                        }
                    },
                    _ => {
                        let vs = i;
                        while i < n && !is_ws(b[i]) && b[i] != b'>' {
                            i += 1;
                        }
                        (&self.src[vs..i], i)
                    }
                };
                i = next;
                value = decode_refs(truncate(raw, MAX_ATTR_VALUE), true).into_owned();
            }
            if tag.attrs.len() < MAX_ATTRS && !tag.attrs.iter().any(|(k, _)| *k == aname) {
                tag.attrs.push((aname, value));
            }
        }
    }
}

impl Iterator for Tokenizer {
    type Item = Token;
    fn next(&mut self) -> Option<Token> {
        match self.next_token() {
            Token::Eof => None,
            t => Some(t),
        }
    }
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn lower_trunc(s: &str, max: usize) -> String {
    truncate(s, max).to_ascii_lowercase()
}

/// Decodes character references (`&amp;`, `&#233;`, `&#x1F600;`, and the
/// legacy `&copy` without ';'). In attribute values a legacy reference
/// followed by a letter, digit or '=' stays as written (so URLs like
/// `?a=1&copy=2` survive), as the spec requires.
pub fn decode_refs(s: &str, in_attr: bool) -> Cow<'_, str> {
    if !s.contains('&') {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        let (text, used) = char_ref(after, in_attr);
        match text {
            Some(t) => {
                out.push_str(&t);
                rest = &after[used..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    Cow::Owned(out)
}

/// One reference after '&': (replacement, bytes consumed).
fn char_ref(s: &str, in_attr: bool) -> (Option<String>, usize) {
    let b = s.as_bytes();
    if b.first() == Some(&b'#') {
        let (hex, start) = match b.get(1) {
            Some(b'x') | Some(b'X') => (true, 2),
            _ => (false, 1),
        };
        let mut i = start;
        let mut v: u32 = 0;
        while i < b.len() && (if hex { b[i].is_ascii_hexdigit() } else { b[i].is_ascii_digit() }) {
            let d = (b[i] as char).to_digit(16).unwrap_or(0);
            v = v.saturating_mul(if hex { 16 } else { 10 }).saturating_add(d);
            i += 1;
        }
        if i == start {
            return (None, 0);
        }
        if b.get(i) == Some(&b';') {
            i += 1;
        }
        let c = match v {
            0 => '\u{FFFD}',
            0xD800..=0xDFFF => '\u{FFFD}',
            v if v > 0x10FFFF => '\u{FFFD}',
            v => entities::c1_replacement(v).or_else(|| char::from_u32(v)).unwrap_or('\u{FFFD}'),
        };
        return (Some(c.to_string()), i);
    }
    let mut i = 0;
    while i < b.len() && b[i].is_ascii_alphanumeric() && i < 40 {
        i += 1;
    }
    if i == 0 {
        return (None, 0);
    }
    let name = &s[..i];
    if b.get(i) == Some(&b';') {
        if let Some(t) = entities::lookup(name) {
            return (Some(t.to_string()), i + 1);
        }
    }
    if let Some((lname, text)) = entities::longest_legacy_prefix(name) {
        let next = b.get(lname.len()).copied();
        if in_attr && next.is_some_and(|c| c.is_ascii_alphanumeric() || c == b'=') {
            return (None, 0);
        }
        return (Some(text.to_string()), lname.len());
    }
    (None, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(s: &str) -> Vec<Token> {
        Tokenizer::new(s).collect()
    }

    fn tag(name: &str, attrs: &[(&str, &str)]) -> Tag {
        Tag { name: name.into(), attrs: attrs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect(), self_closing: false }
    }

    #[test]
    fn tags_and_text() {
        let t = toks("<P CLASS=a id='b' data-x=\"c d\" checked>Hi</p>");
        assert_eq!(
            t,
            vec![
                Token::StartTag(tag("p", &[("class", "a"), ("id", "b"), ("data-x", "c d"), ("checked", "")])),
                Token::Text("Hi".into()),
                Token::EndTag(tag("p", &[])),
            ]
        );
    }

    #[test]
    fn duplicate_attributes_keep_the_first() {
        let t = toks("<a href=1 href=2>");
        assert_eq!(t, vec![Token::StartTag(tag("a", &[("href", "1")]))]);
    }

    #[test]
    fn character_references() {
        assert_eq!(decode_refs("a &amp; b &lt;&gt; &quot;x&quot;", false), "a & b <> \"x\"");
        assert_eq!(decode_refs("&#233;&#xE9;&#X41;&#0;&#x110000;&#xD800;", false), "ééA\u{FFFD}\u{FFFD}\u{FFFD}");
        assert_eq!(decode_refs("&#150;&#128;", false), "\u{2013}\u{20AC}");
        assert_eq!(decode_refs("&copy 2024 &notit; &unknown; & &;", false), "\u{A9} 2024 \u{AC}it; &unknown; & &;");
        assert_eq!(decode_refs("?a=1&copy=2&amp=3&lt", true), "?a=1&copy=2&amp=3<");
        assert_eq!(decode_refs("&#99999999999999999999;", false), "\u{FFFD}");
        assert_eq!(decode_refs("&#;&#x;", false), "&#;&#x;");
    }

    #[test]
    fn raw_text_elements() {
        let t = toks("<script>if (a<b) document.write('</p>')</script><style>p{}</style><title>a &amp; b</title>");
        assert_eq!(t[1], Token::Text("if (a<b) document.write('</p>')".into()));
        assert_eq!(t[2], Token::EndTag(tag("script", &[])));
        assert_eq!(t[4], Token::Text("p{}".into()));
        assert_eq!(t[7], Token::Text("a & b".into()));
        let t = toks("<script>never closed <b>");
        assert_eq!(t[1], Token::Text("never closed <b>".into()));
    }

    #[test]
    fn comments_doctypes_and_bogus() {
        let t = toks("<!DOCTYPE html><!-- c --><!--><?xml x?><!x></>a<");
        assert_eq!(t[0], Token::Doctype { name: "html".into(), quirks: false });
        assert_eq!(t[1], Token::Comment(" c ".into()));
        assert_eq!(t[2], Token::Comment("".into()));
        assert_eq!(t[3], Token::Comment("?xml x?".into()));
        assert_eq!(t[4], Token::Comment("x".into()));
        assert_eq!(t[5], Token::Text("a".into()));
        assert_eq!(t[6], Token::Text("<".into()));
        assert_eq!(t.len(), 7);
    }

    #[test]
    fn self_closing_and_eof_in_tag() {
        assert_eq!(toks("<br/>"), vec![Token::StartTag(Tag { name: "br".into(), attrs: vec![], self_closing: true })]);
        assert_eq!(toks("text<a href='x"), vec![Token::Text("text".into())]);
        assert_eq!(toks("1 < 2 <3"), vec![Token::Text("1 ".into()), Token::Text("< 2 ".into()), Token::Text("<3".into())]);
    }

    #[test]
    fn limits_hold() {
        let mut s = String::from("<x");
        for i in 0..10_000 {
            s += &format!(" a{i}=1");
        }
        s += &format!(" big=\"{}\">", "y".repeat(1_000_000));
        let t = toks(&s);
        match &t[0] {
            Token::StartTag(t) => assert_eq!(t.attrs.len(), MAX_ATTRS),
            other => panic!("{other:?}"),
        }
        let s = format!("<{}>", "n".repeat(100_000));
        match &toks(&s)[0] {
            Token::StartTag(t) => assert_eq!(t.name.len(), MAX_NAME),
            other => panic!("{other:?}"),
        }
    }
}
