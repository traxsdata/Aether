//! URLs: a strict subset of the WHATWG URL Standard for http and https.
//!
//! - Schemes: http and https only (others are an error the browser can show).
//! - Hosts: ASCII DNS names (lowercased) or dotted-decimal IPv4. Non-ASCII
//!   hosts (IDNA), IPv6 literals and user:password@ are rejected.
//! - Paths, queries and fragments are percent-encoded as the standard says,
//!   backslashes count as slashes, and "." / ".." segments are resolved.
//! - Relative references are resolved against a base with `join`.

use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UrlError {
    Empty,
    /// Not http or https (the scheme, lowercased).
    UnsupportedScheme(String),
    /// A relative reference with nothing to resolve it against.
    NoScheme,
    MissingHost,
    InvalidHost(String),
    /// Hosts must be ASCII (internationalized names aren't supported yet).
    NonAsciiHost,
    Ipv6Unsupported,
    InvalidPort,
    /// user:password@host is refused (it is mostly used to disguise links).
    Credentials,
    TooLong,
}

impl fmt::Display for UrlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UrlError::Empty => write!(f, "the address is empty"),
            UrlError::UnsupportedScheme(s) => write!(f, "\"{s}:\" addresses are not supported (only http and https)"),
            UrlError::NoScheme => write!(f, "the address is incomplete (no http:// or https://)"),
            UrlError::MissingHost => write!(f, "the address has no host name"),
            UrlError::InvalidHost(h) => write!(f, "\"{h}\" is not a valid host name"),
            UrlError::NonAsciiHost => write!(f, "host names with non-ASCII characters are not supported"),
            UrlError::Ipv6Unsupported => write!(f, "IPv6 addresses are not supported"),
            UrlError::InvalidPort => write!(f, "the port number is invalid"),
            UrlError::Credentials => write!(f, "addresses with a user name or password are not allowed"),
            UrlError::TooLong => write!(f, "the address is too long"),
        }
    }
}

impl std::error::Error for UrlError {}

pub const MAX_URL_LEN: usize = 8192;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Url {
    serialized: String,
    scheme: &'static str,
    host: String,
    port: Option<u16>,
    path: String,
    query: Option<String>,
    fragment: Option<String>,
}

fn default_port(scheme: &str) -> u16 {
    if scheme == "https" {
        443
    } else {
        80
    }
}

fn is_slash(c: char) -> bool {
    c == '/' || c == '\\'
}

/// Characters that must be percent-encoded, per component.
#[derive(Clone, Copy)]
enum Set {
    Path,
    Query,
    Fragment,
}

fn needs_encoding(b: u8, set: Set) -> bool {
    if b <= 0x20 || b >= 0x7f {
        return true;
    }
    match set {
        Set::Fragment => matches!(b, b'"' | b'<' | b'>' | b'`'),
        Set::Query => matches!(b, b'"' | b'#' | b'<' | b'>' | b'\''),
        Set::Path => matches!(b, b'"' | b'#' | b'<' | b'>' | b'?' | b'`' | b'{' | b'}'),
    }
}

fn encode(s: &str, set: Set) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if needs_encoding(b, set) {
            out.push_str(&format!("%{b:02X}"));
        } else {
            out.push(b as char);
        }
    }
    out
}

/// Percent-decodes (for comparing dot segments and for form fields).
pub fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let h = |c: u8| (c as char).to_digit(16);
            if let (Some(x), Some(y)) = (h(b[i + 1]), h(b[i + 2])) {
                out.push((x * 16 + y) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// application/x-www-form-urlencoded serialization of one name or value.
pub fn form_encode(s: &str) -> String {
    let mut out = String::new();
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'*' | b'-' | b'.' | b'_' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Encodes form fields as application/x-www-form-urlencoded.
pub fn form_urlencoded(fields: &[(&str, &str)]) -> String {
    fields.iter().map(|(k, v)| format!("{}={}", form_encode(k), form_encode(v))).collect::<Vec<_>>().join("&")
}

fn is_single_dot(seg: &str) -> bool {
    seg == "." || seg.eq_ignore_ascii_case("%2e")
}

fn is_double_dot(seg: &str) -> bool {
    matches!(seg.to_ascii_lowercase().as_str(), ".." | ".%2e" | "%2e." | "%2e%2e")
}

/// Normalizes an absolute path ("/a/./b/../c" -> "/a/c"), percent-encoding it.
fn normalize_path(raw: &str) -> String {
    let raw = raw.replace('\\', "/");
    let raw = raw.strip_prefix('/').unwrap_or(&raw);
    let segs: Vec<&str> = raw.split('/').collect();
    let mut out: Vec<String> = Vec::new();
    let n = segs.len();
    for (i, seg) in segs.iter().enumerate() {
        let last = i == n - 1;
        if is_double_dot(seg) {
            out.pop();
            if last {
                out.push(String::new());
            }
        } else if is_single_dot(seg) {
            if last {
                out.push(String::new());
            }
        } else {
            out.push(encode(seg, Set::Path));
        }
    }
    format!("/{}", out.join("/"))
}

fn parse_host(h: &str) -> Result<String, UrlError> {
    if h.is_empty() {
        return Err(UrlError::MissingHost);
    }
    if h.starts_with('[') {
        return Err(UrlError::Ipv6Unsupported);
    }
    if !h.is_ascii() {
        return Err(UrlError::NonAsciiHost);
    }
    let lower = h.to_ascii_lowercase();
    let host = lower.strip_suffix('.').unwrap_or(&lower);
    let bad = || UrlError::InvalidHost(h.to_string());
    if host.is_empty() || host.len() > 253 {
        return Err(bad());
    }
    let labels: Vec<&str> = host.split('.').collect();
    for l in &labels {
        if l.is_empty() || l.len() > 63 || !l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
            return Err(bad());
        }
    }
    // A name ending in a number is an IPv4 address, and then must be a
    // plain dotted quad (no hex, octal or shortened forms).
    let last = labels[labels.len() - 1];
    if (last.bytes().all(|b| b.is_ascii_digit()) || last.starts_with("0x"))
        && crate::x509::parse_ip(host).is_none() {
            return Err(bad());
        }
    Ok(host.to_string())
}

fn split_scheme(s: &str) -> Option<(String, &str)> {
    let colon = s.find(':')?;
    let scheme = &s[..colon];
    let mut chars = scheme.chars();
    let first = chars.next()?;
    if !first.is_ascii_alphabetic() || !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')) {
        return None;
    }
    Some((scheme.to_ascii_lowercase(), &s[colon + 1..]))
}

/// Splits "path?query#fragment" (all optional).
fn split_pqf(s: &str) -> (&str, Option<&str>, Option<&str>) {
    let (rest, frag) = match s.find('#') {
        Some(i) => (&s[..i], Some(&s[i + 1..])),
        None => (s, None),
    };
    let (path, query) = match rest.find('?') {
        Some(i) => (&rest[..i], Some(&rest[i + 1..])),
        None => (rest, None),
    };
    (path, query, frag)
}

/// Removes leading and trailing C0 controls and spaces, and all tabs and
/// newlines (as the standard does).
fn clean(s: &str) -> String {
    s.trim_matches(|c: char| c <= ' ').chars().filter(|&c| c != '\t' && c != '\n' && c != '\r').collect()
}

impl Url {
    /// Parses an absolute http or https URL.
    pub fn parse(s: &str) -> Result<Url, UrlError> {
        Self::parse_with(s, None)
    }

    /// What the user typed in the address bar: "example.com/x" means
    /// https://example.com/x. A scheme given explicitly is kept.
    pub fn from_user_input(s: &str) -> Result<Url, UrlError> {
        let s = clean(s);
        if s.is_empty() {
            return Err(UrlError::Empty);
        }
        let lower = s.to_ascii_lowercase();
        if s.contains("://") || lower.starts_with("http:") || lower.starts_with("https:") {
            Self::parse(&s)
        } else {
            Self::parse(&format!("https://{s}"))
        }
    }

    /// Resolves `rel` (a link or redirect target) against this URL.
    pub fn join(&self, rel: &str) -> Result<Url, UrlError> {
        Self::parse_with(rel, Some(self))
    }

    fn parse_with(input: &str, base: Option<&Url>) -> Result<Url, UrlError> {
        if input.len() > MAX_URL_LEN {
            return Err(UrlError::TooLong);
        }
        let s = clean(input);
        if s.is_empty() {
            return match base {
                Some(b) => Ok(b.without_fragment()),
                None => Err(UrlError::Empty),
            };
        }
        if let Some((scheme, rest)) = split_scheme(&s) {
            if let Some(b) = base {
                // "http:foo" relative to an http URL is a relative reference.
                if scheme == b.scheme && !rest.starts_with(is_slash) {
                    return Self::relative(rest, b);
                }
            }
            let scheme: &'static str = match scheme.as_str() {
                "http" => "http",
                "https" => "https",
                _ => return Err(UrlError::UnsupportedScheme(scheme)),
            };
            return Self::absolute(scheme, rest);
        }
        match base {
            Some(b) => Self::relative(&s, b),
            None => Err(UrlError::NoScheme),
        }
    }

    /// `rest` is everything after "scheme:".
    fn absolute(scheme: &'static str, rest: &str) -> Result<Url, UrlError> {
        let rest = rest.trim_start_matches(is_slash);
        let end = rest.find(|c| is_slash(c) || c == '?' || c == '#').unwrap_or(rest.len());
        let authority = &rest[..end];
        let after = &rest[end..];
        if authority.contains('@') {
            return Err(UrlError::Credentials);
        }
        let (host, port) = if authority.starts_with('[') {
            return Err(UrlError::Ipv6Unsupported);
        } else {
            match authority.rfind(':') {
                Some(i) => {
                    let p = &authority[i + 1..];
                    let port = if p.is_empty() {
                        None
                    } else {
                        if p.len() > 5 || !p.bytes().all(|b| b.is_ascii_digit()) {
                            return Err(UrlError::InvalidPort);
                        }
                        let v: u32 = p.parse().map_err(|_| UrlError::InvalidPort)?;
                        if v == 0 || v > 65535 {
                            return Err(UrlError::InvalidPort);
                        }
                        Some(v as u16)
                    };
                    (&authority[..i], port)
                }
                None => (authority, None),
            }
        };
        let host = parse_host(host)?;
        let port = port.filter(|&p| p != default_port(scheme));
        let (path, query, frag) = split_pqf(after);
        Ok(Self::build(scheme, host, port, normalize_path(path), query, frag))
    }

    fn relative(rel: &str, base: &Url) -> Result<Url, UrlError> {
        let mut chars = rel.chars();
        let (c0, c1) = (chars.next(), chars.next());
        if c0.is_some_and(is_slash) && c1.is_some_and(is_slash) {
            return Self::absolute(base.scheme, rel);
        }
        let (path, query, frag) = split_pqf(rel);
        let (path, query) = if c0.is_some_and(is_slash) {
            (normalize_path(path), query.map(|q| q.to_string()))
        } else if path.is_empty() {
            // "?q" keeps the path; "#f" keeps path and query.
            let q = if query.is_some() { query.map(|q| q.to_string()) } else { base.query.clone() };
            (base.path.clone(), q)
        } else {
            let dir = match base.path.rfind('/') {
                Some(i) => &base.path[..=i],
                None => "/",
            };
            (normalize_path(&format!("{dir}{path}")), query.map(|q| q.to_string()))
        };
        Ok(Self::build_encoded_path(base.scheme, base.host.clone(), base.port, path, query.as_deref(), frag))
    }

    fn build(
        scheme: &'static str,
        host: String,
        port: Option<u16>,
        path: String,
        query: Option<&str>,
        fragment: Option<&str>,
    ) -> Url {
        Self::build_encoded_path(scheme, host, port, path, query, fragment)
    }

    fn build_encoded_path(
        scheme: &'static str,
        host: String,
        port: Option<u16>,
        path: String,
        query: Option<&str>,
        fragment: Option<&str>,
    ) -> Url {
        // Encoding is idempotent on already-encoded text ('%' is left alone).
        let query = query.map(|q| encode(q, Set::Query));
        let fragment = fragment.map(|f| encode(f, Set::Fragment));
        let mut s = format!("{scheme}://{host}");
        if let Some(p) = port {
            s.push_str(&format!(":{p}"));
        }
        s.push_str(&path);
        if let Some(q) = &query {
            s.push('?');
            s.push_str(q);
        }
        if let Some(f) = &fragment {
            s.push('#');
            s.push_str(f);
        }
        Url { serialized: s, scheme, host, port, path, query, fragment }
    }

    pub fn as_str(&self) -> &str {
        &self.serialized
    }
    pub fn scheme(&self) -> &str {
        self.scheme
    }
    pub fn host(&self) -> &str {
        &self.host
    }
    /// The explicit port, if it isn't the scheme's default.
    pub fn port(&self) -> Option<u16> {
        self.port
    }
    pub fn port_or_default(&self) -> u16 {
        self.port.unwrap_or(default_port(self.scheme))
    }
    pub fn path(&self) -> &str {
        &self.path
    }
    pub fn query(&self) -> Option<&str> {
        self.query.as_deref()
    }
    pub fn fragment(&self) -> Option<&str> {
        self.fragment.as_deref()
    }
    pub fn is_https(&self) -> bool {
        self.scheme == "https"
    }

    /// Path and query, as sent in an HTTP request line.
    pub fn request_target(&self) -> String {
        match &self.query {
            Some(q) => format!("{}?{}", self.path, q),
            None => self.path.clone(),
        }
    }

    /// host, or host:port when the port isn't the default (the Host header).
    pub fn authority(&self) -> String {
        match self.port {
            Some(p) => format!("{}:{}", self.host, p),
            None => self.host.clone(),
        }
    }

    pub fn without_fragment(&self) -> Url {
        let mut u = self.clone();
        if u.fragment.take().is_some() {
            if let Some(i) = u.serialized.find('#') {
                u.serialized.truncate(i);
            }
        }
        u
    }
}

impl fmt::Display for Url {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.serialized)
    }
}

impl std::str::FromStr for Url {
    type Err = UrlError;
    fn from_str(s: &str) -> Result<Url, UrlError> {
        Url::parse(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> String {
        Url::parse(s).unwrap().as_str().to_string()
    }

    #[test]
    fn basics() {
        assert_eq!(p("https://Example.COM"), "https://example.com/");
        assert_eq!(p("https://example.com:443/a"), "https://example.com/a");
        assert_eq!(p("http://example.com:8080/a?b#c"), "http://example.com:8080/a?b#c");
        assert_eq!(p("HTTP://example.com/a b"), "http://example.com/a%20b");
        assert_eq!(p("https://example.com/a/./b/../c"), "https://example.com/a/c");
        assert_eq!(p("https://example.com/a/%2e%2E/c"), "https://example.com/c");
        assert_eq!(p("https://example.com/../../x"), "https://example.com/x");
        assert_eq!(p("https:\\\\example.com\\a\\b"), "https://example.com/a/b");
        assert_eq!(p("https://example.com/é?q=ü"), "https://example.com/%C3%A9?q=%C3%BC");
        assert_eq!(p("  https://example.com/\t\n "), "https://example.com/");
        assert_eq!(p("https://example.com./"), "https://example.com/");
        assert_eq!(p("http://10.0.0.1:81"), "http://10.0.0.1:81/");
        let u = Url::parse("https://example.com:8443/p?q=1#f").unwrap();
        assert_eq!(u.host(), "example.com");
        assert_eq!(u.port_or_default(), 8443);
        assert_eq!(u.request_target(), "/p?q=1");
        assert_eq!(u.authority(), "example.com:8443");
        assert_eq!(u.without_fragment().as_str(), "https://example.com:8443/p?q=1");
    }

    #[test]
    fn rejects() {
        assert!(matches!(Url::parse("ftp://x.com"), Err(UrlError::UnsupportedScheme(_))));
        assert!(matches!(Url::parse("javascript:alert(1)"), Err(UrlError::UnsupportedScheme(_))));
        assert_eq!(Url::parse("example.com"), Err(UrlError::NoScheme));
        assert_eq!(Url::parse("https://user:pw@example.com/"), Err(UrlError::Credentials));
        assert_eq!(Url::parse("https://exämple.com/"), Err(UrlError::NonAsciiHost));
        assert_eq!(Url::parse("https://[::1]/"), Err(UrlError::Ipv6Unsupported));
        assert_eq!(Url::parse("https://example.com:99999/"), Err(UrlError::InvalidPort));
        assert_eq!(Url::parse("https://example.com:0/"), Err(UrlError::InvalidPort));
        assert_eq!(Url::parse("https://"), Err(UrlError::MissingHost));
        assert!(Url::parse("https://exa mple.com/").is_err());
        assert!(Url::parse("https://a..b/").is_err());
        assert!(Url::parse("https://0x7f.1/").is_err());
        assert!(Url::parse("https://127.1/").is_err());
        assert!(Url::parse("https://example.123/").is_err());
        assert!(Url::parse("https://ex%41mple.com/").is_err());
    }

    #[test]
    fn user_input() {
        assert_eq!(Url::from_user_input("example.com").unwrap().as_str(), "https://example.com/");
        assert_eq!(Url::from_user_input("example.com/a?b").unwrap().as_str(), "https://example.com/a?b");
        assert_eq!(Url::from_user_input("localhost:8443").unwrap().as_str(), "https://localhost:8443/");
        assert_eq!(Url::from_user_input("http://example.com").unwrap().as_str(), "http://example.com/");
        assert!(Url::from_user_input("  ").is_err());
    }

    #[test]
    fn join_rfc3986_examples() {
        // RFC 3986 section 5.4.1, adapted to http.
        let b = Url::parse("http://a/b/c/d;p?q").unwrap();
        let cases = [
            ("g", "http://a/b/c/g"),
            ("./g", "http://a/b/c/g"),
            ("g/", "http://a/b/c/g/"),
            ("/g", "http://a/g"),
            ("//g", "http://g/"),
            ("?y", "http://a/b/c/d;p?y"),
            ("g?y", "http://a/b/c/g?y"),
            ("#s", "http://a/b/c/d;p?q#s"),
            ("g#s", "http://a/b/c/g#s"),
            ("g?y#s", "http://a/b/c/g?y#s"),
            (";x", "http://a/b/c/;x"),
            ("g;x", "http://a/b/c/g;x"),
            ("", "http://a/b/c/d;p?q"),
            (".", "http://a/b/c/"),
            ("./", "http://a/b/c/"),
            ("..", "http://a/b/"),
            ("../", "http://a/b/"),
            ("../g", "http://a/b/g"),
            ("../..", "http://a/"),
            ("../../g", "http://a/g"),
            ("../../../g", "http://a/g"),
            ("/./g", "http://a/g"),
            ("/../g", "http://a/g"),
            ("g.", "http://a/b/c/g."),
            ("..g", "http://a/b/c/..g"),
            ("./../g", "http://a/b/g"),
            ("g/./h", "http://a/b/c/g/h"),
            ("g/../h", "http://a/b/c/h"),
            ("http:g", "http://a/b/c/g"),
            ("https://other.example/x", "https://other.example/x"),
            ("\\\\h\\i", "http://h/i"),
        ];
        for (rel, want) in cases {
            assert_eq!(b.join(rel).unwrap().as_str(), want, "joining {rel:?}");
        }
        assert!(b.join("mailto:x@y").is_err());
    }

    #[test]
    fn forms() {
        assert_eq!(form_urlencoded(&[("q", "a b&c"), ("x", "é")]), "q=a+b%26c&x=%C3%A9");
        assert_eq!(percent_decode("a%20b%zz"), b"a b%zz");
    }
}
