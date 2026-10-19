//! tokenizer.rs - CSS Syntax Level 3 tokenization, and the "component
//! value" tree (blocks and functions) the parser works on.

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Ident(String),
    Function(String),
    AtKeyword(String),
    Hash(String),
    Str(String),
    Url(String),
    BadUrl,
    BadStr,
    Delim(char),
    Number(f32),
    Percentage(f32),
    Dimension(f32, String),
    Ws,
    Cdo,
    Cdc,
    Colon,
    Semicolon,
    Comma,
    LBracket,
    RBracket,
    LParen,
    RParen,
    LBrace,
    RBrace,
}

/// A component value: a token, a {} [] () block, or a function.
#[derive(Clone, Debug, PartialEq)]
pub enum Cv {
    Tok(Tok),
    Block(char, Vec<Cv>),
    Func(String, Vec<Cv>),
}

impl Cv {
    pub fn is_ws(&self) -> bool {
        matches!(self, Cv::Tok(Tok::Ws))
    }
    pub fn ident(&self) -> Option<&str> {
        match self {
            Cv::Tok(Tok::Ident(s)) => Some(s),
            _ => None,
        }
    }
}

/// Maximum nesting of blocks and functions; deeper content is dropped.
pub const MAX_NEST: usize = 32;
/// Maximum tokens per stylesheet (so a hostile sheet costs bounded memory).
pub const MAX_TOKENS: usize = 1_000_000;

pub fn tokenize(src: &str) -> Vec<Tok> {
    let mut t = Lexer { s: src.as_bytes(), src, i: 0 };
    let mut out = Vec::new();
    while let Some(tok) = t.next() {
        if out.len() >= MAX_TOKENS {
            break;
        }
        out.push(tok);
    }
    out
}

struct Lexer<'a> {
    s: &'a [u8],
    src: &'a str,
    i: usize,
}

fn is_name_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c >= 0x80
}
fn is_name(c: u8) -> bool {
    is_name_start(c) || c.is_ascii_digit() || c == b'-'
}

impl Lexer<'_> {
    fn peek(&self, k: usize) -> Option<u8> {
        self.s.get(self.i + k).copied()
    }

    fn valid_escape(&self, k: usize) -> bool {
        self.peek(k) == Some(b'\\') && !matches!(self.peek(k + 1), Some(b'\n') | None)
    }

    fn starts_ident(&self, k: usize) -> bool {
        match self.peek(k) {
            Some(b'-') => matches!(self.peek(k + 1), Some(c) if is_name_start(c) || c == b'-') || self.valid_escape(k + 1),
            Some(c) if is_name_start(c) => true,
            Some(b'\\') => self.valid_escape(k),
            _ => false,
        }
    }

    fn starts_number(&self) -> bool {
        match self.peek(0) {
            Some(b'+') | Some(b'-') => {
                matches!(self.peek(1), Some(c) if c.is_ascii_digit()) || (self.peek(1) == Some(b'.') && matches!(self.peek(2), Some(c) if c.is_ascii_digit()))
            }
            Some(b'.') => matches!(self.peek(1), Some(c) if c.is_ascii_digit()),
            Some(c) => c.is_ascii_digit(),
            None => false,
        }
    }

    /// One character (whole UTF-8 sequence) at i.
    fn take_char(&mut self) -> char {
        let c = self.src[self.i..].chars().next().unwrap_or('\u{FFFD}');
        self.i += c.len_utf8();
        c
    }

    fn escape(&mut self) -> char {
        // At the character after '\'.
        let mut hex = 0u32;
        let mut n = 0;
        while n < 6 {
            match self.peek(0) {
                Some(c) if c.is_ascii_hexdigit() => {
                    hex = hex * 16 + (c as char).to_digit(16).unwrap_or(0);
                    self.i += 1;
                    n += 1;
                }
                _ => break,
            }
        }
        if n > 0 {
            if matches!(self.peek(0), Some(b' ' | b'\t' | b'\n')) {
                self.i += 1;
            }
            return match hex {
                0 | 0xD800..=0xDFFF => '\u{FFFD}',
                h => char::from_u32(h).unwrap_or('\u{FFFD}'),
            };
        }
        if self.i >= self.s.len() {
            return '\u{FFFD}';
        }
        self.take_char()
    }

    fn name(&mut self) -> String {
        let mut out = String::new();
        loop {
            match self.peek(0) {
                Some(c) if is_name(c) => {
                    out.push(self.take_char());
                }
                Some(b'\\') if self.valid_escape(0) => {
                    self.i += 1;
                    out.push(self.escape());
                }
                _ => break,
            }
            if out.len() > 1024 {
                // Absurd identifiers: consume the rest, keep a prefix.
                while matches!(self.peek(0), Some(c) if is_name(c)) {
                    self.i += 1;
                }
                while !self.src.is_char_boundary(self.i) {
                    self.i += 1;
                }
                break;
            }
        }
        out
    }

    fn number(&mut self) -> f32 {
        let start = self.i;
        if matches!(self.peek(0), Some(b'+' | b'-')) {
            self.i += 1;
        }
        while matches!(self.peek(0), Some(c) if c.is_ascii_digit()) {
            self.i += 1;
        }
        if self.peek(0) == Some(b'.') && matches!(self.peek(1), Some(c) if c.is_ascii_digit()) {
            self.i += 1;
            while matches!(self.peek(0), Some(c) if c.is_ascii_digit()) {
                self.i += 1;
            }
        }
        if matches!(self.peek(0), Some(b'e' | b'E')) {
            let k = if matches!(self.peek(1), Some(b'+' | b'-')) { 2 } else { 1 };
            if matches!(self.peek(k), Some(c) if c.is_ascii_digit()) {
                self.i += k;
                while matches!(self.peek(0), Some(c) if c.is_ascii_digit()) {
                    self.i += 1;
                }
            }
        }
        let v: f32 = self.src[start..self.i].parse().unwrap_or(0.0);
        if v.is_finite() {
            v.clamp(-1e7, 1e7)
        } else {
            0.0
        }
    }

    fn string(&mut self, q: u8) -> Tok {
        self.i += 1;
        let mut out = String::new();
        loop {
            match self.peek(0) {
                None => return Tok::Str(out),
                Some(c) if c == q => {
                    self.i += 1;
                    return Tok::Str(out);
                }
                Some(b'\n') => return Tok::BadStr,
                Some(b'\\') => {
                    self.i += 1;
                    match self.peek(0) {
                        None => {}
                        Some(b'\n') => self.i += 1,
                        _ => out.push(self.escape()),
                    }
                }
                _ => out.push(self.take_char()),
            }
        }
    }

    fn url(&mut self) -> Tok {
        while matches!(self.peek(0), Some(b' ' | b'\t' | b'\n')) {
            self.i += 1;
        }
        let mut out = String::new();
        loop {
            match self.peek(0) {
                None => return Tok::Url(out),
                Some(b')') => {
                    self.i += 1;
                    return Tok::Url(out);
                }
                Some(b' ' | b'\t' | b'\n') => {
                    while matches!(self.peek(0), Some(b' ' | b'\t' | b'\n')) {
                        self.i += 1;
                    }
                    if matches!(self.peek(0), Some(b')') | None) {
                        self.i = (self.i + 1).min(self.s.len());
                        return Tok::Url(out);
                    }
                    return self.bad_url();
                }
                Some(b'"' | b'\'' | b'(') => return self.bad_url(),
                Some(b'\\') => {
                    if self.valid_escape(0) {
                        self.i += 1;
                        out.push(self.escape());
                    } else {
                        return self.bad_url();
                    }
                }
                _ => out.push(self.take_char()),
            }
        }
    }

    fn bad_url(&mut self) -> Tok {
        while let Some(c) = self.peek(0) {
            self.i += 1;
            if c == b')' {
                break;
            }
            if c == b'\\' && self.peek(0).is_some() {
                self.i += 1;
            }
        }
        while !self.src.is_char_boundary(self.i.min(self.s.len())) {
            self.i += 1;
        }
        self.i = self.i.min(self.s.len());
        Tok::BadUrl
    }

    fn numeric(&mut self) -> Tok {
        let v = self.number();
        if self.starts_ident(0) {
            let unit = self.name().to_ascii_lowercase();
            Tok::Dimension(v, unit)
        } else if self.peek(0) == Some(b'%') {
            self.i += 1;
            Tok::Percentage(v)
        } else {
            Tok::Number(v)
        }
    }

    fn ident_like(&mut self) -> Tok {
        let name = self.name();
        if self.peek(0) == Some(b'(') {
            self.i += 1;
            if name.eq_ignore_ascii_case("url") {
                let mut k = 0;
                while matches!(self.peek(k), Some(b' ' | b'\t' | b'\n')) {
                    k += 1;
                }
                if !matches!(self.peek(k), Some(b'"' | b'\'')) {
                    return self.url();
                }
            }
            return Tok::Function(name.to_ascii_lowercase());
        }
        Tok::Ident(name)
    }

    fn next(&mut self) -> Option<Tok> {
        // Comments.
        loop {
            if self.peek(0) == Some(b'/') && self.peek(1) == Some(b'*') {
                match self.src[self.i + 2..].find("*/") {
                    Some(j) => self.i += 2 + j + 2,
                    None => self.i = self.s.len(),
                }
            } else {
                break;
            }
        }
        let c = self.peek(0)?;
        Some(match c {
            b' ' | b'\t' | b'\n' | b'\r' | b'\x0C' => {
                while matches!(self.peek(0), Some(b' ' | b'\t' | b'\n' | b'\r' | b'\x0C')) {
                    self.i += 1;
                }
                Tok::Ws
            }
            b'"' | b'\'' => self.string(c),
            b'#' => {
                if matches!(self.peek(1), Some(c) if is_name(c)) || self.valid_escape(1) {
                    self.i += 1;
                    Tok::Hash(self.name())
                } else {
                    self.i += 1;
                    Tok::Delim('#')
                }
            }
            b'(' => {
                self.i += 1;
                Tok::LParen
            }
            b')' => {
                self.i += 1;
                Tok::RParen
            }
            b'[' => {
                self.i += 1;
                Tok::LBracket
            }
            b']' => {
                self.i += 1;
                Tok::RBracket
            }
            b'{' => {
                self.i += 1;
                Tok::LBrace
            }
            b'}' => {
                self.i += 1;
                Tok::RBrace
            }
            b',' => {
                self.i += 1;
                Tok::Comma
            }
            b':' => {
                self.i += 1;
                Tok::Colon
            }
            b';' => {
                self.i += 1;
                Tok::Semicolon
            }
            b'+' | b'.' if self.starts_number() => self.numeric(),
            b'-' => {
                if self.starts_number() {
                    self.numeric()
                } else if self.peek(1) == Some(b'-') && self.peek(2) == Some(b'>') {
                    self.i += 3;
                    Tok::Cdc
                } else if self.starts_ident(0) {
                    self.ident_like()
                } else {
                    self.i += 1;
                    Tok::Delim('-')
                }
            }
            b'<' if self.s[self.i..].starts_with(b"<!--") => {
                self.i += 4;
                Tok::Cdo
            }
            b'@' => {
                self.i += 1;
                if self.starts_ident(0) {
                    Tok::AtKeyword(self.name().to_ascii_lowercase())
                } else {
                    Tok::Delim('@')
                }
            }
            b'\\' => {
                if self.valid_escape(0) {
                    self.ident_like()
                } else {
                    self.i += 1;
                    Tok::Delim('\\')
                }
            }
            c if c.is_ascii_digit() => self.numeric(),
            c if is_name_start(c) => self.ident_like(),
            _ => Tok::Delim(self.take_char()),
        })
    }
}

/// Builds the component value tree. Unbalanced closers are dropped, open
/// blocks are closed at the end, and nesting deeper than MAX_NEST is
/// flattened away (its contents are skipped).
pub fn component_values(toks: Vec<Tok>) -> Vec<Cv> {
    // Iterative, with an explicit stack: (closing token, function name, contents).
    let mut stack: Vec<(Tok, Option<String>, char, Vec<Cv>)> = Vec::new();
    let mut top: Vec<Cv> = Vec::new();
    let mut skip_depth = 0usize;
    for t in toks {
        if skip_depth > 0 {
            match t {
                Tok::LBrace | Tok::LBracket | Tok::LParen | Tok::Function(_) => skip_depth += 1,
                Tok::RBrace | Tok::RBracket | Tok::RParen => skip_depth -= 1,
                _ => {}
            }
            continue;
        }
        let open = match &t {
            Tok::LBrace => Some((Tok::RBrace, None, '{')),
            Tok::LBracket => Some((Tok::RBracket, None, '[')),
            Tok::LParen => Some((Tok::RParen, None, '(')),
            Tok::Function(name) => Some((Tok::RParen, Some(name.clone()), '(')),
            _ => None,
        };
        if let Some((close, fname, ch)) = open {
            if stack.len() >= MAX_NEST {
                skip_depth = 1;
                continue;
            }
            stack.push((close, fname, ch, std::mem::take(&mut top)));
            continue;
        }
        if let Some((close, _, _, _)) = stack.last() {
            if *close == t {
                let (_, fname, ch, outer) = stack.pop().unwrap();
                let inner = std::mem::replace(&mut top, outer);
                top.push(match fname {
                    Some(n) => Cv::Func(n, inner),
                    None => Cv::Block(ch, inner),
                });
                continue;
            }
        }
        match t {
            // A stray closer at the top level (or a mismatched one) is ignored.
            Tok::RBrace | Tok::RBracket | Tok::RParen if stack.is_empty() => {}
            t => top.push(Cv::Tok(t)),
        }
    }
    while let Some((_, fname, ch, outer)) = stack.pop() {
        let inner = std::mem::replace(&mut top, outer);
        top.push(match fname {
            Some(n) => Cv::Func(n, inner),
            None => Cv::Block(ch, inner),
        });
    }
    top
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens() {
        let t = tokenize("a.b #id{color:red;width:10.5px;w:50%;x:1e2} /* c */ @media url(x.png) url( 'y' ) \"s\\\"q\" -moz-x --var u+");
        assert!(t.contains(&Tok::Hash("id".into())));
        assert!(t.contains(&Tok::Dimension(10.5, "px".into())));
        assert!(t.contains(&Tok::Percentage(50.0)));
        assert!(t.contains(&Tok::Number(100.0)));
        assert!(t.contains(&Tok::AtKeyword("media".into())));
        assert!(t.contains(&Tok::Url("x.png".into())));
        assert!(t.contains(&Tok::Function("url".into())));
        assert!(t.contains(&Tok::Str("s\"q".into())));
        assert!(t.contains(&Tok::Ident("-moz-x".into())));
        assert!(t.contains(&Tok::Ident("--var".into())));
        assert_eq!(tokenize("\\31 0"), vec![Tok::Ident("10".into())]);
    }

    #[test]
    fn blocks() {
        let cv = component_values(tokenize("a { b: f(1, [2]) } }"));
        assert_eq!(cv.len(), 4);
        match &cv[2] {
            Cv::Block('{', inner) => assert!(inner.iter().any(|c| matches!(c, Cv::Func(n, _) if n == "f"))),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn hostile() {
        let deep = "(".repeat(100_000) + &")".repeat(100_000);
        let cv = component_values(tokenize(&deep));
        assert!(cv.len() <= 1);
        let _ = tokenize("/* never closed");
        let _ = tokenize("url(  never closed");
        let _ = tokenize("\"never closed");
        let _ = tokenize("\\");
        let _ = tokenize("é\\ü\\\n#\\");
        let _ = component_values(tokenize(&"{[(".repeat(50_000)));
        let _ = tokenize(&"a".repeat(100_000));
    }
}
