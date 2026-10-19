//! script/regex.rs - regular expressions: a backtracking matcher with a
//! step budget, so a pattern can't hang the page however it's written.
//!
//! Supported: alternation, groups (capturing, non-capturing, named),
//! lookahead, backreferences, greedy and lazy quantifiers, classes with
//! ranges and \d \w \s, anchors and \b, and the flags g, i, m, s, u and y.
//! Not supported (a SyntaxError): lookbehind and Unicode property escapes.
//! Positions count characters (code points).

#[derive(Debug, Clone)]
enum ClassItem {
    Range(char, char),
    Digit(bool),
    Word(bool),
    Space(bool),
}

#[derive(Debug, Clone)]
struct Class {
    items: Vec<ClassItem>,
    negated: bool,
}

#[derive(Debug, Clone)]
enum Node {
    Char(char),
    Any,
    Class(Box<Class>),
    Start,
    End,
    WordBoundary(bool),
    Group(Box<Node>, Option<usize>),
    Look(Box<Node>, bool),
    BackRef(usize),
    Seq(Vec<Node>),
    Alt(Vec<Node>),
    Repeat { node: Box<Node>, min: u32, max: u32, greedy: bool },
}

#[derive(Debug)]
pub struct Regex {
    pub source: String,
    pub flags: String,
    pub global: bool,
    pub ignore_case: bool,
    pub multiline: bool,
    pub dot_all: bool,
    pub sticky: bool,
    node: Node,
    pub ncaps: usize,
    pub names: Vec<(String, usize)>,
}

/// Captures: [0] is the whole match.
pub type Caps = Vec<Option<(usize, usize)>>;

const STEP_LIMIT: u64 = 2_000_000;
const DEPTH_LIMIT: usize = 3000;
const MAX_PATTERN: usize = 32 * 1024;

pub enum MatchError {
    TooComplex,
}

struct P<'a> {
    s: &'a [char],
    i: usize,
    ncaps: usize,
    names: Vec<(String, usize)>,
    depth: usize,
    icase: bool,
}

impl Regex {
    pub fn new(source: &str, flags: &str) -> Result<Regex, String> {
        if source.len() > MAX_PATTERN {
            return Err("pattern too long".into());
        }
        let mut seen = String::new();
        for f in flags.chars() {
            if !"gimsuyd".contains(f) || seen.contains(f) {
                return Err(format!("invalid flags '{flags}'"));
            }
            seen.push(f);
        }
        let chars: Vec<char> = source.chars().collect();
        let mut p = P { s: &chars, i: 0, ncaps: 1, names: Vec::new(), depth: 0, icase: flags.contains('i') };
        let node = p.alt()?;
        if p.i < chars.len() {
            return Err("unmatched ')'".into());
        }
        Ok(Regex {
            source: source.to_string(),
            flags: flags.to_string(),
            global: flags.contains('g'),
            ignore_case: flags.contains('i'),
            multiline: flags.contains('m'),
            dot_all: flags.contains('s'),
            sticky: flags.contains('y'),
            ncaps: p.ncaps,
            names: p.names,
            node,
        })
    }

    /// The first match at or after `start` (exactly at `start` if sticky).
    pub fn exec_at(&self, text: &[char], start: usize) -> Result<Option<Caps>, MatchError> {
        let mut m = M { re: self, s: text, caps: vec![None; self.ncaps], steps: 0, depth: 0 };
        let mut at = start;
        while at <= text.len() {
            for c in m.caps.iter_mut() {
                *c = None;
            }
            let mut end = None;
            let node = &self.node;
            let ok = m.node(node, at, &mut |_, j| {
                end = Some(j);
                true
            })?;
            if ok {
                let mut caps = m.caps.clone();
                caps[0] = Some((at, end.unwrap_or(at)));
                return Ok(Some(caps));
            }
            if self.sticky {
                break;
            }
            at += 1;
        }
        Ok(None)
    }
}

impl<'a> P<'a> {
    fn peek(&self) -> Option<char> {
        self.s.get(self.i).copied()
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn alt(&mut self) -> Result<Node, String> {
        self.depth += 1;
        if self.depth > 200 {
            return Err("pattern nested too deeply".into());
        }
        let mut alts = vec![self.seq()?];
        while self.eat('|') {
            alts.push(self.seq()?);
        }
        self.depth -= 1;
        Ok(if alts.len() == 1 { alts.pop().unwrap() } else { Node::Alt(alts) })
    }

    fn seq(&mut self) -> Result<Node, String> {
        let mut items = Vec::new();
        while let Some(c) = self.peek() {
            if c == '|' || c == ')' {
                break;
            }
            let atom = self.atom()?;
            let atom = self.quantifier(atom)?;
            items.push(atom);
        }
        Ok(if items.len() == 1 { items.pop().unwrap() } else { Node::Seq(items) })
    }

    fn number(&mut self) -> Option<u32> {
        let start = self.i;
        let mut v: u32 = 0;
        while let Some(d) = self.peek().and_then(|c| c.to_digit(10)) {
            v = v.saturating_mul(10).saturating_add(d);
            self.i += 1;
        }
        if self.i == start {
            None
        } else {
            Some(v.min(100_000))
        }
    }

    fn quantifier(&mut self, atom: Node) -> Result<Node, String> {
        let (min, max) = match self.peek() {
            Some('*') => {
                self.i += 1;
                (0, u32::MAX)
            }
            Some('+') => {
                self.i += 1;
                (1, u32::MAX)
            }
            Some('?') => {
                self.i += 1;
                (0, 1)
            }
            Some('{') => {
                let save = self.i;
                self.i += 1;
                match self.number() {
                    Some(lo) => {
                        let hi = if self.eat(',') { self.number().unwrap_or(u32::MAX) } else { lo };
                        if !self.eat('}') {
                            self.i = save;
                            return Ok(atom);
                        }
                        if hi < lo {
                            return Err("numbers out of order in {} quantifier".into());
                        }
                        (lo, hi)
                    }
                    None => {
                        // A literal '{'.
                        self.i = save;
                        return Ok(atom);
                    }
                }
            }
            _ => return Ok(atom),
        };
        if matches!(atom, Node::Start | Node::End | Node::WordBoundary(_) | Node::Look(..)) {
            return Err("nothing to repeat".into());
        }
        let greedy = !self.eat('?');
        Ok(Node::Repeat { node: Box::new(atom), min, max, greedy })
    }

    fn atom(&mut self) -> Result<Node, String> {
        let c = self.peek().unwrap();
        self.i += 1;
        Ok(match c {
            '.' => Node::Any,
            '^' => Node::Start,
            '$' => Node::End,
            '*' | '+' | '?' => return Err("nothing to repeat".into()),
            '[' => Node::Class(Box::new(self.class()?)),
            '(' => {
                if self.eat('?') {
                    if self.eat(':') {
                        let n = self.alt()?;
                        self.close()?;
                        Node::Group(Box::new(n), None)
                    } else if self.eat('=') || self.eat('!') {
                        let neg = self.s[self.i - 1] == '!';
                        let n = self.alt()?;
                        self.close()?;
                        Node::Look(Box::new(n), neg)
                    } else if self.eat('<') {
                        if self.peek() == Some('=') || self.peek() == Some('!') {
                            return Err("lookbehind is not supported".into());
                        }
                        let mut name = String::new();
                        while let Some(c) = self.peek() {
                            self.i += 1;
                            if c == '>' {
                                break;
                            }
                            name.push(c);
                        }
                        let idx = self.ncaps;
                        self.ncaps += 1;
                        self.names.push((name, idx));
                        let n = self.alt()?;
                        self.close()?;
                        Node::Group(Box::new(n), Some(idx))
                    } else {
                        return Err("invalid group".into());
                    }
                } else {
                    let idx = self.ncaps;
                    self.ncaps += 1;
                    if self.ncaps > 1000 {
                        return Err("too many groups".into());
                    }
                    let n = self.alt()?;
                    self.close()?;
                    Node::Group(Box::new(n), Some(idx))
                }
            }
            ')' => return Err("unmatched ')'".into()),
            '\\' => self.escape(false)?,
            c => Node::Char(c),
        })
    }

    fn close(&mut self) -> Result<(), String> {
        if self.eat(')') {
            Ok(())
        } else {
            Err("unterminated group".into())
        }
    }

    fn hex(&mut self, n: usize) -> Option<u32> {
        let mut v = 0;
        for k in 0..n {
            let d = self.s.get(self.i + k)?.to_digit(16)?;
            v = v * 16 + d;
        }
        self.i += n;
        Some(v)
    }

    /// After a backslash. In a class, \b is a backspace and there are no backreferences.
    fn escape(&mut self, in_class: bool) -> Result<Node, String> {
        let Some(c) = self.peek() else { return Err("\\ at end of pattern".into()) };
        self.i += 1;
        let ch = |c: char| Ok(Node::Char(c));
        match c {
            'd' => Ok(Node::Class(Box::new(Class { items: vec![ClassItem::Digit(true)], negated: false }))),
            'D' => Ok(Node::Class(Box::new(Class { items: vec![ClassItem::Digit(false)], negated: false }))),
            'w' => Ok(Node::Class(Box::new(Class { items: vec![ClassItem::Word(true)], negated: false }))),
            'W' => Ok(Node::Class(Box::new(Class { items: vec![ClassItem::Word(false)], negated: false }))),
            's' => Ok(Node::Class(Box::new(Class { items: vec![ClassItem::Space(true)], negated: false }))),
            'S' => Ok(Node::Class(Box::new(Class { items: vec![ClassItem::Space(false)], negated: false }))),
            'b' if in_class => ch('\u{8}'),
            'b' => Ok(Node::WordBoundary(true)),
            'B' => Ok(Node::WordBoundary(false)),
            'n' => ch('\n'),
            'r' => ch('\r'),
            't' => ch('\t'),
            'f' => ch('\u{C}'),
            'v' => ch('\u{B}'),
            '0' => ch('\0'),
            'c' => match self.peek() {
                Some(l) if l.is_ascii_alphabetic() => {
                    self.i += 1;
                    ch(char::from_u32(l as u32 % 32).unwrap_or('\0'))
                }
                _ => ch('\\'),
            },
            'x' => match self.hex(2) {
                Some(v) => ch(char::from_u32(v).unwrap_or('\u{FFFD}')),
                None => ch('x'),
            },
            'u' => {
                if self.peek() == Some('{') {
                    let save = self.i;
                    self.i += 1;
                    let mut v = 0u32;
                    while let Some(d) = self.peek().and_then(|c| c.to_digit(16)) {
                        v = v.saturating_mul(16).saturating_add(d);
                        self.i += 1;
                    }
                    if self.eat('}') {
                        return ch(char::from_u32(v).unwrap_or('\u{FFFD}'));
                    }
                    self.i = save;
                    return ch('u');
                }
                match self.hex(4) {
                    Some(hi) if (0xD800..0xDC00).contains(&hi) && self.peek() == Some('\\') && self.s.get(self.i + 1) == Some(&'u') => {
                        let save = self.i;
                        self.i += 2;
                        match self.hex(4) {
                            Some(lo) if (0xDC00..0xE000).contains(&lo) => ch(char::from_u32(0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)).unwrap_or('\u{FFFD}')),
                            _ => {
                                self.i = save;
                                ch('\u{FFFD}')
                            }
                        }
                    }
                    Some(v) => ch(char::from_u32(v).unwrap_or('\u{FFFD}')),
                    None => ch('u'),
                }
            }
            'p' | 'P' if self.peek() == Some('{') => Err("Unicode property escapes are not supported".into()),
            'k' if !in_class && self.peek() == Some('<') => {
                self.i += 1;
                let mut name = String::new();
                while let Some(c) = self.peek() {
                    self.i += 1;
                    if c == '>' {
                        break;
                    }
                    name.push(c);
                }
                // Named groups may come later in the pattern; resolve by name at the end
                // is overkill, so look them up among those already seen.
                match self.names.iter().find(|(n, _)| *n == name) {
                    Some((_, i)) => Ok(Node::BackRef(*i)),
                    None => Err(format!("invalid named reference '{name}'")),
                }
            }
            '1'..='9' if !in_class => {
                self.i -= 1;
                let n = self.number().unwrap_or(0) as usize;
                Ok(Node::BackRef(n))
            }
            c => ch(c),
        }
    }

    fn class(&mut self) -> Result<Class, String> {
        let negated = self.eat('^');
        let mut items = Vec::new();
        let mut first = true;
        loop {
            let Some(c) = self.peek() else { return Err("unterminated character class".into()) };
            if c == ']' && !(first && false) {
                self.i += 1;
                break;
            }
            first = false;
            let lo = self.class_atom()?;
            if self.peek() == Some('-') && self.s.get(self.i + 1).is_some_and(|&c| c != ']') {
                if let ClassItem::Range(a, _) = lo {
                    self.i += 1;
                    let hi = self.class_atom()?;
                    match hi {
                        ClassItem::Range(b, _) => {
                            if b < a {
                                return Err("range out of order in character class".into());
                            }
                            items.push(ClassItem::Range(a, b));
                        }
                        other => {
                            items.push(ClassItem::Range(a, a));
                            items.push(ClassItem::Range('-', '-'));
                            items.push(other);
                        }
                    }
                    continue;
                }
            }
            items.push(lo);
        }
        if self.icase {
            // Add the other case of each letter range.
            let mut extra = Vec::new();
            for it in &items {
                if let ClassItem::Range(a, b) = it {
                    if (*b as u32) - (*a as u32) < 512 {
                        for c in *a..=*b {
                            let l = fold(c);
                            let u = c.to_uppercase().next().unwrap_or(c);
                            if l != c {
                                extra.push(ClassItem::Range(l, l));
                            }
                            if u != c {
                                extra.push(ClassItem::Range(u, u));
                            }
                        }
                    }
                }
            }
            items.extend(extra);
        }
        Ok(Class { items, negated })
    }

    fn class_atom(&mut self) -> Result<ClassItem, String> {
        let c = self.peek().unwrap();
        self.i += 1;
        if c == '\\' {
            return Ok(match self.escape(true)? {
                Node::Char(c) => ClassItem::Range(c, c),
                Node::Class(cl) => cl.items.into_iter().next().unwrap_or(ClassItem::Range('\0', '\0')),
                _ => ClassItem::Range('\u{8}', '\u{8}'),
            });
        }
        Ok(ClassItem::Range(c, c))
    }
}

fn fold(c: char) -> char {
    if c.is_ascii() {
        c.to_ascii_lowercase()
    } else {
        c.to_lowercase().next().unwrap_or(c)
    }
}

fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn is_space(c: char) -> bool {
    c.is_whitespace() || c == '\u{FEFF}'
}

fn is_line_term(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

struct M<'a> {
    re: &'a Regex,
    s: &'a [char],
    caps: Caps,
    steps: u64,
    depth: usize,
}

type K<'k, 'a> = &'k mut dyn FnMut(&mut M<'a>, usize) -> bool;

impl<'a> M<'a> {
    fn class_has(&self, cl: &Class, c: char) -> bool {
        let fc = if self.re.ignore_case { fold(c) } else { c };
        let hit = cl.items.iter().any(|it| match it {
            ClassItem::Range(a, b) => (*a <= c && c <= *b) || (*a <= fc && fc <= *b),
            ClassItem::Digit(p) => c.is_ascii_digit() == *p,
            ClassItem::Word(p) => is_word(c) == *p,
            ClassItem::Space(p) => is_space(c) == *p,
        });
        hit != cl.negated
    }

    /// Whether a single-character node matches s[i].
    fn one(&self, n: &Node, i: usize) -> bool {
        let Some(&c) = self.s.get(i) else { return false };
        match n {
            Node::Char(x) => c == *x || (self.re.ignore_case && fold(c) == fold(*x)),
            Node::Any => self.re.dot_all || !is_line_term(c),
            Node::Class(cl) => self.class_has(cl, c),
            _ => false,
        }
    }

    fn tick(&mut self) -> Result<(), MatchError> {
        self.steps += 1;
        if self.steps > STEP_LIMIT {
            return Err(MatchError::TooComplex);
        }
        Ok(())
    }

    fn node(&mut self, n: &'a Node, i: usize, k: K<'_, 'a>) -> Result<bool, MatchError> {
        self.tick()?;
        self.depth += 1;
        if self.depth > DEPTH_LIMIT {
            self.depth -= 1;
            return Err(MatchError::TooComplex);
        }
        let r = self.node_inner(n, i, k);
        self.depth -= 1;
        r
    }

    fn node_inner(&mut self, n: &'a Node, i: usize, k: K<'_, 'a>) -> Result<bool, MatchError> {
        match n {
            Node::Char(_) | Node::Any | Node::Class(_) => {
                if self.one(n, i) {
                    Ok(k(self, i + 1))
                } else {
                    Ok(false)
                }
            }
            Node::Start => {
                let ok = i == 0 || (self.re.multiline && is_line_term(self.s[i - 1]));
                Ok(ok && k(self, i))
            }
            Node::End => {
                let ok = i == self.s.len() || (self.re.multiline && is_line_term(self.s[i]));
                Ok(ok && k(self, i))
            }
            Node::WordBoundary(want) => {
                let a = i > 0 && is_word(self.s[i - 1]);
                let b = i < self.s.len() && is_word(self.s[i]);
                Ok(((a != b) == *want) && k(self, i))
            }
            Node::BackRef(g) => {
                let Some(Some((a, b))) = self.caps.get(*g).copied() else { return Ok(k(self, i)) };
                let len = b - a;
                if i + len > self.s.len() {
                    return Ok(false);
                }
                for x in 0..len {
                    let (p, q) = (self.s[a + x], self.s[i + x]);
                    if p != q && !(self.re.ignore_case && fold(p) == fold(q)) {
                        return Ok(false);
                    }
                }
                Ok(k(self, i + len))
            }
            Node::Group(inner, idx) => match idx {
                None => self.node(inner, i, k),
                Some(g) => {
                    let g = *g;
                    let old = self.caps[g];
                    let r = self.node(inner, i, &mut |m: &mut M<'a>, j| {
                        let before = m.caps[g];
                        m.caps[g] = Some((i, j));
                        if k(m, j) {
                            true
                        } else {
                            m.caps[g] = before;
                            false
                        }
                    })?;
                    if !r {
                        self.caps[g] = old;
                    }
                    Ok(r)
                }
            },
            Node::Look(inner, neg) => {
                let saved = self.caps.clone();
                let found = self.node(inner, i, &mut |_, _| true)?;
                if found == *neg {
                    self.caps = saved;
                    return Ok(false);
                }
                if *neg {
                    self.caps = saved;
                }
                Ok(k(self, i))
            }
            Node::Seq(items) => self.seq(items, 0, i, k),
            Node::Alt(alts) => {
                for a in alts {
                    if self.node(a, i, k)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            Node::Repeat { node, min, max, greedy } => {
                if matches!(**node, Node::Char(_) | Node::Any | Node::Class(_)) {
                    // A single character repeated: count, then back off without recursion.
                    let mut n = 0usize;
                    while (n as u64) < *max as u64 && self.one(node, i + n) {
                        n += 1;
                        self.tick()?;
                    }
                    if n < *min as usize {
                        return Ok(false);
                    }
                    if *greedy {
                        let mut c = n;
                        loop {
                            self.tick()?;
                            if k(self, i + c) {
                                return Ok(true);
                            }
                            if c == *min as usize {
                                return Ok(false);
                            }
                            c -= 1;
                        }
                    } else {
                        for c in *min as usize..=n {
                            self.tick()?;
                            if k(self, i + c) {
                                return Ok(true);
                            }
                        }
                        return Ok(false);
                    }
                }
                self.repeat(node, *min, *max, *greedy, 0, i, usize::MAX, k)
            }
        }
    }

    fn seq(&mut self, items: &'a [Node], at: usize, i: usize, k: K<'_, 'a>) -> Result<bool, MatchError> {
        if at == items.len() {
            return Ok(k(self, i));
        }
        let mut err = None;
        let r = self.node(&items[at], i, &mut |m: &mut M<'a>, j| match m.seq(items, at + 1, j, k) {
            Ok(v) => v,
            Err(e) => {
                err = Some(e);
                true
            }
        })?;
        if let Some(e) = err {
            return Err(e);
        }
        Ok(r)
    }

    #[allow(clippy::too_many_arguments)]
    fn repeat(&mut self, node: &'a Node, min: u32, max: u32, greedy: bool, count: u32, i: usize, last: usize, k: K<'_, 'a>) -> Result<bool, MatchError> {
        // An iteration that matched nothing ends the loop (no infinite loops on empty matches).
        if count > min && last == i {
            return Ok(false);
        }
        let mut err = None;
        if count < min {
            let r = self.more(node, min, max, greedy, count, i, k, &mut err)?;
            if let Some(e) = err {
                return Err(e);
            }
            return Ok(r);
        }
        if greedy {
            let r = self.more(node, min, max, greedy, count, i, k, &mut err)?;
            if let Some(e) = err.take() {
                return Err(e);
            }
            if r {
                return Ok(true);
            }
            Ok(k(self, i))
        } else {
            if k(self, i) {
                return Ok(true);
            }
            let r = self.more(node, min, max, greedy, count, i, k, &mut err)?;
            if let Some(e) = err {
                return Err(e);
            }
            Ok(r)
        }
    }

    /// One more iteration of a repeat, then the rest of the repeat.
    #[allow(clippy::too_many_arguments)]
    fn more(&mut self, node: &'a Node, min: u32, max: u32, greedy: bool, count: u32, i: usize, k: K<'_, 'a>, err: &mut Option<MatchError>) -> Result<bool, MatchError> {
        if count >= max {
            return Ok(false);
        }
        self.node(node, i, &mut |m: &mut M<'a>, j| match m.repeat(node, min, max, greedy, count + 1, j, i, k) {
            Ok(v) => v,
            Err(e) => {
                *err = Some(e);
                true
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find(p: &str, f: &str, s: &str) -> Option<(usize, usize)> {
        let re = Regex::new(p, f).unwrap();
        let chars: Vec<char> = s.chars().collect();
        re.exec_at(&chars, 0).ok().unwrap().map(|c| c[0].unwrap())
    }

    fn caps(p: &str, s: &str) -> Vec<Option<String>> {
        let re = Regex::new(p, "").unwrap();
        let chars: Vec<char> = s.chars().collect();
        let c = re.exec_at(&chars, 0).ok().unwrap().unwrap();
        c.iter().map(|x| x.map(|(a, b)| chars[a..b].iter().collect())).collect()
    }

    #[test]
    fn matching() {
        assert_eq!(find("b+", "", "aabbbc"), Some((2, 5)));
        assert_eq!(find("b+?", "", "aabbbc"), Some((2, 3)));
        assert_eq!(find("^a|c$", "", "xc"), Some((1, 2)));
        assert_eq!(find("[a-c]{2,3}", "", "xxabcd"), Some((2, 5)));
        assert_eq!(find("\\bfoo\\b", "", "a foo b"), Some((2, 5)));
        assert_eq!(find("FOO", "i", "a foo"), Some((2, 5)));
        assert_eq!(find("[^\\d\\s]+", "", "12 ab3"), Some((3, 5)));
        assert_eq!(find("a(?=b)", "", "acab"), Some((2, 3)));
        assert_eq!(find("a(?!b)", "", "abac"), Some((2, 3)));
        assert_eq!(find("^b", "m", "a\nb"), Some((2, 3)));
        assert_eq!(find("a.c", "", "a\nc"), None);
        assert_eq!(find("a.c", "s", "a\nc"), Some((0, 3)));
        assert_eq!(find("(a|ab)(c|bcd)(d*)", "", "abcd"), Some((0, 4)));
        assert_eq!(caps("(\\w+)@(\\w+)\\.com", "me@x.com"), vec![Some("me@x.com".into()), Some("me".into()), Some("x".into())]);
        assert_eq!(caps("(a)|(b)", "b"), vec![Some("b".into()), None, Some("b".into())]);
        assert_eq!(caps("(\\w)\\1", "abccd")[0], Some("cc".into()));
        assert_eq!(caps("(?<y>\\d{4})-\\k<y>", "2024-2024")[1], Some("2024".into()));
        assert_eq!(find("(a*)*b", "", "aaab"), Some((0, 4)));
        assert_eq!(find("x{", "", "x{"), Some((0, 2)));
        assert_eq!(find("[\\]a]+", "", "]a]"), Some((0, 3)));
    }

    #[test]
    fn errors_and_limits() {
        assert!(Regex::new("(", "").is_err());
        assert!(Regex::new(")", "").is_err());
        assert!(Regex::new("*", "").is_err());
        assert!(Regex::new("a", "gg").is_err());
        assert!(Regex::new("(?<=a)b", "").is_err());
        // Catastrophic backtracking stops instead of hanging.
        let re = Regex::new("(a+)+b", "").unwrap();
        let s: Vec<char> = "a".repeat(40).chars().collect();
        assert!(re.exec_at(&s, 0).is_err());
        // A long input with a simple repeat doesn't recurse.
        let long: Vec<char> = "a".repeat(200_000).chars().collect();
        let re = Regex::new("a*$", "").unwrap();
        assert!(matches!(re.exec_at(&long, 0), Ok(Some(_))));
    }
}
