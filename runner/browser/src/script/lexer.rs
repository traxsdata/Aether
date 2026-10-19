//! script/lexer.rs - source text to tokens.
//!
//! The whole script is tokenized up front. Whether a `/` starts a regular
//! expression or divides is decided from the token before it, the usual
//! approach for a tokenizer that runs ahead of the parser. Template literals
//! become one token holding their string parts and the tokens of each
//! `${...}` expression.

use std::rc::Rc;

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    /// Identifiers and keywords alike; the parser tells them apart.
    Ident(Rc<str>),
    Num(f64),
    Str(Rc<str>),
    /// `cooked` and `raw` have one more entry than `exprs`.
    Template { cooked: Vec<Rc<str>>, raw: Vec<Rc<str>>, exprs: Vec<Vec<Token>> },
    Regex(Rc<str>, Rc<str>),
    /// Punctuation and operators.
    P(&'static str),
    /// `#name` (private class members).
    Private(Rc<str>),
    Eof,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    pub tok: Tok,
    /// A line break comes between this token and the one before it.
    pub nl: bool,
    /// Line number, from 1.
    pub line: u32,
}

#[derive(Debug, Clone)]
pub struct LexError {
    pub msg: String,
    pub line: u32,
}

const PUNCT: &[&str] = &[
    ">>>=", "...", "===", "!==", "**=", "<<=", ">>=", ">>>", "&&=", "||=", "??=", "=>", "==", "!=", "<=", ">=", "&&", "||", "??", "?.", "++", "--",
    "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "**", "<<", ">>", "{", "}", "(", ")", "[", "]", ";", ",", "<", ">", "+", "-", "*", "/", "%", "&", "|",
    "^", "!", "~", "?", ":", "=", ".", "@",
];

/// Keywords after which a `/` starts a regular expression.
const REGEX_AFTER: &[&str] = &["return", "typeof", "instanceof", "in", "of", "new", "delete", "void", "throw", "case", "do", "else", "yield", "await"];

/// Largest script, in bytes, and deepest template nesting.
pub const MAX_SOURCE: usize = 8 << 20;
const MAX_TEMPLATE_DEPTH: usize = 32;

pub fn tokenize(src: &str) -> Result<Vec<Token>, LexError> {
    if src.len() > MAX_SOURCE {
        return Err(LexError { msg: "script too large".into(), line: 1 });
    }
    let chars: Vec<char> = src.chars().collect();
    let mut lx = Lexer { s: &chars, i: 0, line: 1, depth: 0 };
    let mut out = lx.run(false)?;
    out.push(Token { tok: Tok::Eof, nl: true, line: lx.line });
    Ok(out)
}

struct Lexer<'a> {
    s: &'a [char],
    i: usize,
    line: u32,
    depth: usize,
}

fn is_id_start(c: char) -> bool {
    c == '$' || c == '_' || c.is_ascii_alphabetic() || (!c.is_ascii() && c.is_alphabetic())
}

fn is_id_part(c: char) -> bool {
    is_id_start(c) || c.is_ascii_digit() || c == '\u{200C}' || c == '\u{200D}' || (!c.is_ascii() && c.is_alphanumeric())
}

fn is_line_term(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

impl<'a> Lexer<'a> {
    fn peek(&self, o: usize) -> char {
        self.s.get(self.i + o).copied().unwrap_or('\0')
    }

    fn err<T>(&self, msg: &str) -> Result<T, LexError> {
        Err(LexError { msg: msg.into(), line: self.line })
    }

    fn regex_ok(prev: Option<&Token>) -> bool {
        match prev.map(|t| &t.tok) {
            None => true,
            Some(Tok::P(p)) => !matches!(*p, ")" | "]"),
            Some(Tok::Ident(k)) => REGEX_AFTER.contains(&&**k),
            _ => false,
        }
    }

    /// Tokens until the end, or (inside a template) until the `}` that
    /// closes the substitution.
    fn run(&mut self, in_template: bool) -> Result<Vec<Token>, LexError> {
        let mut out: Vec<Token> = Vec::new();
        let mut nl = false;
        let mut braces = 0usize;
        let mut line_start = true;
        loop {
            let c = self.peek(0);
            if self.i >= self.s.len() {
                if in_template {
                    return self.err("unterminated template literal");
                }
                return Ok(out);
            }
            // White space and comments.
            if is_line_term(c) {
                if c == '\r' && self.peek(1) == '\n' {
                    self.i += 1;
                }
                self.i += 1;
                self.line += 1;
                nl = true;
                line_start = true;
                continue;
            }
            if c.is_whitespace() || c == '\u{FEFF}' {
                self.i += 1;
                continue;
            }
            if c == '/' && self.peek(1) == '/' || (c == '<' && self.peek(1) == '!' && self.peek(2) == '-' && self.peek(3) == '-') || (line_start && c == '-' && self.peek(1) == '-' && self.peek(2) == '>') {
                while self.i < self.s.len() && !is_line_term(self.peek(0)) {
                    self.i += 1;
                }
                continue;
            }
            if c == '/' && self.peek(1) == '*' {
                self.i += 2;
                loop {
                    if self.i >= self.s.len() {
                        return self.err("unterminated comment");
                    }
                    let d = self.peek(0);
                    if d == '*' && self.peek(1) == '/' {
                        self.i += 2;
                        break;
                    }
                    if is_line_term(d) {
                        self.line += 1;
                        nl = true;
                    }
                    self.i += 1;
                }
                continue;
            }
            line_start = false;
            let line = self.line;
            let tok = if is_id_start(c) || c == '\\' {
                Tok::Ident(self.ident()?)
            } else if c.is_ascii_digit() || (c == '.' && self.peek(1).is_ascii_digit()) {
                Tok::Num(self.number()?)
            } else if c == '"' || c == '\'' {
                Tok::Str(self.string(c)?)
            } else if c == '`' {
                self.template()?
            } else if c == '#' && is_id_start(self.peek(1)) {
                self.i += 1;
                Tok::Private(self.ident()?)
            } else if c == '/' && Self::regex_ok(out.last()) {
                self.regex()?
            } else {
                let mut found = None;
                for p in PUNCT {
                    let n = p.chars().count();
                    if p.chars().enumerate().all(|(k, pc)| self.peek(k) == pc) {
                        // `?.` before a digit is `?` then a number (a ? .5 : 1).
                        if *p == "?." && self.peek(2).is_ascii_digit() {
                            continue;
                        }
                        found = Some((*p, n));
                        break;
                    }
                }
                let Some((p, n)) = found else {
                    return self.err(&format!("unexpected character {c:?}"));
                };
                self.i += n;
                if in_template {
                    if p == "{" {
                        braces += 1;
                    } else if p == "}" {
                        if braces == 0 {
                            return Ok(out);
                        }
                        braces -= 1;
                    }
                }
                Tok::P(p)
            };
            out.push(Token { tok, nl, line });
            nl = false;
        }
    }

    fn ident(&mut self) -> Result<Rc<str>, LexError> {
        let mut s = String::new();
        loop {
            let c = self.peek(0);
            if c == '\\' && self.peek(1) == 'u' {
                self.i += 2;
                let ch = self.unicode_escape()?;
                s.push(ch);
                continue;
            }
            if self.i < self.s.len() && is_id_part(c) {
                s.push(c);
                self.i += 1;
            } else {
                break;
            }
        }
        if s.is_empty() {
            return self.err("bad identifier");
        }
        Ok(s.into())
    }

    fn number(&mut self) -> Result<f64, LexError> {
        let c = self.peek(0);
        let radix = if c == '0' {
            match self.peek(1) {
                'x' | 'X' => 16,
                'o' | 'O' => 8,
                'b' | 'B' => 2,
                _ => 10,
            }
        } else {
            10
        };
        if radix != 10 {
            self.i += 2;
            let mut v = 0f64;
            let mut any = false;
            loop {
                let d = self.peek(0);
                if d == '_' {
                    self.i += 1;
                    continue;
                }
                match d.to_digit(radix) {
                    Some(x) => {
                        v = v * radix as f64 + x as f64;
                        any = true;
                        self.i += 1;
                    }
                    None => break,
                }
            }
            if self.peek(0) == 'n' {
                self.i += 1; // BigInt literal: kept as a number
            }
            if !any || is_id_start(self.peek(0)) {
                return self.err("bad number");
            }
            return Ok(v);
        }
        let mut s = String::new();
        // Legacy octal (010) is read as decimal; strict code forbids it anyway.
        while self.peek(0).is_ascii_digit() || self.peek(0) == '_' {
            if self.peek(0) != '_' {
                s.push(self.peek(0));
            }
            self.i += 1;
        }
        if self.peek(0) == '.' {
            s.push('.');
            self.i += 1;
            while self.peek(0).is_ascii_digit() || self.peek(0) == '_' {
                if self.peek(0) != '_' {
                    s.push(self.peek(0));
                }
                self.i += 1;
            }
        }
        if matches!(self.peek(0), 'e' | 'E') && (self.peek(1).is_ascii_digit() || (matches!(self.peek(1), '+' | '-') && self.peek(2).is_ascii_digit())) {
            s.push('e');
            self.i += 1;
            if matches!(self.peek(0), '+' | '-') {
                s.push(self.peek(0));
                self.i += 1;
            }
            while self.peek(0).is_ascii_digit() {
                s.push(self.peek(0));
                self.i += 1;
            }
        }
        if self.peek(0) == 'n' {
            self.i += 1;
        }
        if is_id_start(self.peek(0)) {
            return self.err("bad number");
        }
        if s.starts_with('.') {
            s.insert(0, '0');
        }
        if s.ends_with('.') {
            s.push('0');
        }
        s.parse::<f64>().or_else(|_| self.err("bad number"))
    }

    fn hex(&mut self, n: usize) -> Result<u32, LexError> {
        let mut v = 0u32;
        for _ in 0..n {
            let Some(d) = self.peek(0).to_digit(16) else { return self.err("bad escape") };
            v = v * 16 + d;
            self.i += 1;
        }
        Ok(v)
    }

    /// After `\u`: XXXX or {X...}; surrogate pairs written as two escapes join.
    fn unicode_escape(&mut self) -> Result<char, LexError> {
        let v = if self.peek(0) == '{' {
            self.i += 1;
            let mut v = 0u32;
            let mut n = 0;
            while self.peek(0) != '}' {
                let Some(d) = self.peek(0).to_digit(16) else { return self.err("bad escape") };
                v = v.saturating_mul(16).saturating_add(d);
                self.i += 1;
                n += 1;
                if n > 8 {
                    return self.err("bad escape");
                }
            }
            self.i += 1;
            v
        } else {
            self.hex(4)?
        };
        if (0xD800..0xDC00).contains(&v) && self.peek(0) == '\\' && self.peek(1) == 'u' {
            let save = self.i;
            self.i += 2;
            if let Ok(lo) = self.hex(4) {
                if (0xDC00..0xE000).contains(&lo) {
                    return Ok(char::from_u32(0x10000 + ((v - 0xD800) << 10) + (lo - 0xDC00)).unwrap_or('\u{FFFD}'));
                }
            }
            self.i = save;
        }
        Ok(char::from_u32(v).unwrap_or('\u{FFFD}'))
    }

    /// One escape after a backslash, into `s`; None for a line continuation.
    fn escape(&mut self, s: &mut String) -> Result<(), LexError> {
        let c = self.peek(0);
        self.i += 1;
        match c {
            'n' => s.push('\n'),
            't' => s.push('\t'),
            'r' => s.push('\r'),
            'b' => s.push('\u{8}'),
            'f' => s.push('\u{C}'),
            'v' => s.push('\u{B}'),
            '0' if !self.peek(0).is_ascii_digit() => s.push('\0'),
            'x' => {
                let v = self.hex(2)?;
                s.push(char::from_u32(v).unwrap_or('\u{FFFD}'));
            }
            'u' => {
                let ch = self.unicode_escape()?;
                s.push(ch);
            }
            '\r' => {
                if self.peek(0) == '\n' {
                    self.i += 1;
                }
                self.line += 1;
            }
            '\n' | '\u{2028}' | '\u{2029}' => self.line += 1,
            c if c.is_ascii_digit() => {
                // Legacy octal escapes.
                let mut v = c.to_digit(8).unwrap_or(0);
                while v < 32 && self.peek(0).is_digit(8) {
                    v = v * 8 + self.peek(0).to_digit(8).unwrap_or(0);
                    self.i += 1;
                }
                s.push(char::from_u32(v).unwrap_or('\u{FFFD}'));
            }
            c => s.push(c),
        }
        Ok(())
    }

    fn string(&mut self, q: char) -> Result<Rc<str>, LexError> {
        self.i += 1;
        let mut s = String::new();
        loop {
            if self.i >= self.s.len() {
                return self.err("unterminated string");
            }
            let c = self.peek(0);
            if c == q {
                self.i += 1;
                return Ok(s.into());
            }
            if c == '\n' || c == '\r' {
                return self.err("unterminated string");
            }
            if c == '\\' {
                self.i += 1;
                self.escape(&mut s)?;
            } else {
                s.push(c);
                self.i += 1;
            }
        }
    }

    fn template(&mut self) -> Result<Tok, LexError> {
        if self.depth >= MAX_TEMPLATE_DEPTH {
            return self.err("template literals nested too deeply");
        }
        self.i += 1;
        let mut cooked = Vec::new();
        let mut raw = Vec::new();
        let mut exprs = Vec::new();
        let mut s = String::new();
        let mut r = String::new();
        loop {
            if self.i >= self.s.len() {
                return self.err("unterminated template literal");
            }
            let c = self.peek(0);
            if c == '`' {
                self.i += 1;
                cooked.push(s.into());
                raw.push(r.into());
                return Ok(Tok::Template { cooked, raw, exprs });
            }
            if c == '$' && self.peek(1) == '{' {
                self.i += 2;
                cooked.push(std::mem::take(&mut s).into());
                raw.push(std::mem::take(&mut r).into());
                self.depth += 1;
                let mut toks = self.run(true)?;
                self.depth -= 1;
                toks.push(Token { tok: Tok::Eof, nl: false, line: self.line });
                exprs.push(toks);
                continue;
            }
            if c == '\\' {
                let start = self.i;
                self.i += 1;
                self.escape(&mut s)?;
                r.extend(&self.s[start..self.i]);
                continue;
            }
            if c == '\n' {
                self.line += 1;
            }
            if c == '\r' {
                // Raw and cooked both normalise CR LF and CR to LF.
                self.i += 1;
                if self.peek(0) == '\n' {
                    self.i += 1;
                }
                self.line += 1;
                s.push('\n');
                r.push('\n');
                continue;
            }
            s.push(c);
            r.push(c);
            self.i += 1;
        }
    }

    fn regex(&mut self) -> Result<Tok, LexError> {
        self.i += 1;
        let mut body = String::new();
        let mut class = false;
        loop {
            let c = self.peek(0);
            if self.i >= self.s.len() || is_line_term(c) {
                return self.err("unterminated regular expression");
            }
            self.i += 1;
            if c == '\\' {
                body.push(c);
                let d = self.peek(0);
                if is_line_term(d) || self.i >= self.s.len() {
                    return self.err("unterminated regular expression");
                }
                body.push(d);
                self.i += 1;
                continue;
            }
            if c == '[' {
                class = true;
            } else if c == ']' {
                class = false;
            } else if c == '/' && !class {
                break;
            }
            body.push(c);
        }
        let mut flags = String::new();
        while is_id_part(self.peek(0)) && self.i < self.s.len() {
            flags.push(self.peek(0));
            self.i += 1;
        }
        Ok(Tok::Regex(body.into(), flags.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(s: &str) -> Vec<Tok> {
        tokenize(s).unwrap().into_iter().map(|t| t.tok).collect()
    }

    #[test]
    fn basics() {
        assert_eq!(toks("a >>>= 0x1F"), vec![Tok::Ident("a".into()), Tok::P(">>>="), Tok::Num(31.0), Tok::Eof]);
        assert_eq!(toks("'a\\n\\x41\\u{1F600}'"), vec![Tok::Str("a\nA\u{1F600}".into()), Tok::Eof]);
        assert_eq!(toks("\"\\uD83D\\uDE00\""), vec![Tok::Str("\u{1F600}".into()), Tok::Eof]);
        assert_eq!(toks("1e3 .5 1_000 5."), vec![Tok::Num(1000.0), Tok::Num(0.5), Tok::Num(1000.0), Tok::Num(5.0), Tok::Eof]);
        assert_eq!(toks("a?.b c ? .5 : 1").len(), 9);
    }

    #[test]
    fn regex_or_divide() {
        assert_eq!(toks("a / b / c").iter().filter(|t| **t == Tok::P("/")).count(), 2);
        assert!(matches!(toks("x = /a[/]b/gi")[2], Tok::Regex(ref b, ref f) if &**b == "a[/]b" && &**f == "gi"));
        assert!(matches!(toks("return /x/")[1], Tok::Regex(..)));
        assert!(matches!(toks("(1) / 2")[3], Tok::P("/")));
    }

    #[test]
    fn templates() {
        let t = toks("`a${b + `c${d}`}e`");
        let Tok::Template { cooked, exprs, .. } = &t[0] else { panic!() };
        assert_eq!(cooked.len(), 2);
        assert_eq!(exprs[0].len(), 4); // b + `...` Eof
        assert!(tokenize("`a${").is_err());
        assert!(tokenize(&"`${".repeat(100)).is_err());
    }

    #[test]
    fn comments_and_lines() {
        let t = tokenize("a // x\n/* y\n */ b <!-- old\n--> c\nd").unwrap();
        assert_eq!(t.len(), 4);
        assert!(t[1].nl);
        assert_eq!(t[1].line, 3);
        assert_eq!(t[2].line, 5);
        assert!(tokenize("'abc").is_err());
        assert!(tokenize("/* abc").is_err());
    }
}
