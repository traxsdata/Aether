//! A small, strict JSON parser for GitHub's release API.
//!
//! It only needs objects, arrays, strings, numbers, booleans and null. Every
//! length and nesting depth is bounded, so a hostile response can't make the
//! installer allocate or recurse without limit.

#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(o) => o.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Array(a) => Some(a),
            _ => None,
        }
    }
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Json::Num(n) if *n >= 0.0 && n.is_finite() => Some(*n as u64),
            _ => None,
        }
    }
}

const MAX_DEPTH: usize = 64;
const MAX_ITEMS: usize = 100_000;

pub fn parse(text: &str) -> Result<Json, String> {
    let mut p = Parser { b: text.as_bytes(), i: 0, items: 0 };
    let v = p.value(0)?;
    p.ws();
    if p.i != p.b.len() {
        return Err(format!("trailing data at byte {}", p.i));
    }
    Ok(v)
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
    items: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\r' | b'\n') {
            self.i += 1;
        }
    }

    fn item(&mut self) -> Result<(), String> {
        self.items += 1;
        if self.items > MAX_ITEMS {
            Err("too many JSON values".into())
        } else {
            Ok(())
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, String> {
        if depth > MAX_DEPTH {
            return Err("JSON is nested too deeply".into());
        }
        self.ws();
        let Some(&c) = self.b.get(self.i) else { return Err("unexpected end of JSON".into()) };
        self.item()?;
        match c {
            b'n' => {
                self.word("null")?;
                Ok(Json::Null)
            }
            b't' => {
                self.word("true")?;
                Ok(Json::Bool(true))
            }
            b'f' => {
                self.word("false")?;
                Ok(Json::Bool(false))
            }
            b'"' => Ok(Json::Str(self.string()?)),
            b'[' => {
                self.i += 1;
                let mut out = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b']') {
                    self.i += 1;
                    return Ok(Json::Array(out));
                }
                loop {
                    out.push(self.value(depth + 1)?);
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(Json::Array(out));
                        }
                        _ => return Err("expected ',' or ']' in JSON array".into()),
                    }
                }
            }
            b'{' => {
                self.i += 1;
                let mut out = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b'}') {
                    self.i += 1;
                    return Ok(Json::Object(out));
                }
                loop {
                    self.ws();
                    if self.b.get(self.i) != Some(&b'"') {
                        return Err("expected a string key in JSON object".into());
                    }
                    let k = self.string()?;
                    self.ws();
                    if self.b.get(self.i) != Some(&b':') {
                        return Err("expected ':' in JSON object".into());
                    }
                    self.i += 1;
                    out.push((k, self.value(depth + 1)?));
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(Json::Object(out));
                        }
                        _ => return Err("expected ',' or '}' in JSON object".into()),
                    }
                }
            }
            b'-' | b'0'..=b'9' => self.number(),
            _ => Err(format!("unexpected byte {} in JSON", c)),
        }
    }

    fn word(&mut self, w: &str) -> Result<(), String> {
        if self.b[self.i..].starts_with(w.as_bytes()) {
            self.i += w.len();
            Ok(())
        } else {
            Err(format!("expected {w}"))
        }
    }

    fn string(&mut self) -> Result<String, String> {
        self.i += 1; // opening quote
        let mut out = String::new();
        loop {
            let Some(&c) = self.b.get(self.i) else { return Err("unterminated JSON string".into()) };
            self.i += 1;
            match c {
                b'"' => return Ok(out),
                b'\\' => {
                    let Some(&e) = self.b.get(self.i) else { return Err("unterminated JSON escape".into()) };
                    self.i += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let h = self.hex4()?;
                            if (0xD800..0xDC00).contains(&h) {
                                // A surrogate pair: expect \uXXXX for the low half.
                                if self.b.get(self.i) == Some(&b'\\') && self.b.get(self.i + 1) == Some(&b'u') {
                                    self.i += 2;
                                    let lo = self.hex4()?;
                                    if (0xDC00..0xE000).contains(&lo) {
                                        let cp = 0x10000 + ((h - 0xD800) << 10) + (lo - 0xDC00);
                                        out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                                    } else {
                                        out.push('\u{FFFD}');
                                    }
                                } else {
                                    out.push('\u{FFFD}');
                                }
                            } else {
                                out.push(char::from_u32(h).unwrap_or('\u{FFFD}'));
                            }
                        }
                        _ => return Err("bad JSON escape".into()),
                    }
                }
                b if b < 0x20 => return Err("control character in JSON string".into()),
                b => out.push(b as char),
            }
            if out.len() > 1_000_000 {
                return Err("JSON string is too long".into());
            }
        }
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let mut v = 0u32;
        for _ in 0..4 {
            let Some(&c) = self.b.get(self.i) else { return Err("short \\u escape".into()) };
            self.i += 1;
            v = v * 16 + (c as char).to_digit(16).ok_or("bad \\u escape")?;
        }
        Ok(v)
    }

    fn number(&mut self) -> Result<Json, String> {
        let start = self.i;
        if self.b.get(self.i) == Some(&b'-') {
            self.i += 1;
        }
        while self.i < self.b.len() && self.b[self.i].is_ascii_digit() {
            self.i += 1;
        }
        if self.b.get(self.i) == Some(&b'.') {
            self.i += 1;
            while self.i < self.b.len() && self.b[self.i].is_ascii_digit() {
                self.i += 1;
            }
        }
        if matches!(self.b.get(self.i), Some(b'e') | Some(b'E')) {
            self.i += 1;
            if matches!(self.b.get(self.i), Some(b'+') | Some(b'-')) {
                self.i += 1;
            }
            while self.i < self.b.len() && self.b[self.i].is_ascii_digit() {
                self.i += 1;
            }
        }
        let s = std::str::from_utf8(&self.b[start..self.i]).map_err(|_| "bad JSON number")?;
        s.parse::<f64>().map(Json::Num).map_err(|_| "bad JSON number".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_release() {
        let text = r#"{"tag_name":"v0.8.0","assets":[{"name":"gridcraft-0.8.0-aether-x86_64.app","browser_download_url":"https://example.test/a.app","size":1234,"digest":"sha256:00"}]}"#;
        let j = parse(text).unwrap();
        assert_eq!(j.get("tag_name").and_then(Json::as_str), Some("v0.8.0"));
        let a = j.get("assets").and_then(Json::as_array).unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].get("size").and_then(Json::as_u64), Some(1234));
    }

    #[test]
    fn rejects_bad_input() {
        assert!(parse("").is_err());
        assert!(parse("{\"a\":}").is_err());
        assert!(parse("[1,2").is_err());
        assert!(parse(&"[".repeat(100)).is_err());
        assert!(parse("\"\\u12\"").is_err());
    }

    #[test]
    fn unescapes_strings() {
        assert_eq!(parse(r#""a\nb\u0041""#).unwrap(), Json::Str("a\nbA".into()));
    }
}
