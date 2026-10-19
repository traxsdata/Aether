//! HTTP/1.1 messages (RFC 9112): writing a request, and strictly reading a
//! response with its body (Content-Length, chunked, or until close).
//! Every length and count is bounded.

use std::io::{self, Read, Write};
use std::time::Instant;

pub const MAX_HEADER_BYTES: usize = 64 * 1024;
pub const MAX_HEADERS: usize = 128;
const MAX_LINE: usize = 16 * 1024;

#[derive(Debug)]
pub enum HttpError {
    Io(io::Error),
    /// The server's response broke the protocol (what was wrong).
    Malformed(&'static str),
    TooLarge,
    Timeout,
    /// The connection closed before the whole response arrived.
    Truncated,
}

impl From<io::Error> for HttpError {
    fn from(e: io::Error) -> Self {
        match e.kind() {
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => HttpError::Timeout,
            io::ErrorKind::UnexpectedEof => HttpError::Truncated,
            _ => HttpError::Io(e),
        }
    }
}

/// A reader with its own buffer, a deadline and line-length limits.
pub struct Conn<S: Read> {
    inner: S,
    buf: Vec<u8>,
    pos: usize,
    end: usize,
    deadline: Instant,
}

impl<S: Read> Conn<S> {
    pub fn new(inner: S, deadline: Instant) -> Self {
        Conn { inner, buf: vec![0; 16 * 1024], pos: 0, end: 0, deadline }
    }

    pub fn get_mut(&mut self) -> &mut S {
        &mut self.inner
    }

    fn fill(&mut self) -> Result<usize, HttpError> {
        if Instant::now() > self.deadline {
            return Err(HttpError::Timeout);
        }
        let n = self.inner.read(&mut self.buf)?;
        self.pos = 0;
        self.end = n;
        Ok(n)
    }

    /// Reads one line (without CRLF or LF). Bare CR is an error.
    fn line(&mut self, limit: usize) -> Result<Vec<u8>, HttpError> {
        let mut out = Vec::new();
        loop {
            if self.pos == self.end && self.fill()? == 0 {
                return Err(HttpError::Truncated);
            }
            let b = self.buf[self.pos];
            self.pos += 1;
            if b == b'\n' {
                if out.last() == Some(&b'\r') {
                    out.pop();
                }
                if out.contains(&b'\r') {
                    return Err(HttpError::Malformed("a bare CR in a header"));
                }
                return Ok(out);
            }
            out.push(b);
            if out.len() > limit {
                return Err(HttpError::TooLarge);
            }
        }
    }

    /// Up to `max` bytes; 0 at end of stream.
    fn read_some(&mut self, max: usize) -> Result<&[u8], HttpError> {
        if self.pos == self.end && self.fill()? == 0 {
            return Ok(&[]);
        }
        let n = max.min(self.end - self.pos);
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    fn read_exact_into(&mut self, mut n: usize, out: &mut Vec<u8>) -> Result<(), HttpError> {
        while n > 0 {
            let s = self.read_some(n)?;
            if s.is_empty() {
                return Err(HttpError::Truncated);
            }
            n -= s.len();
            out.extend_from_slice(s);
        }
        Ok(())
    }
}

pub struct Head {
    pub status: u16,
    pub headers: Vec<(String, String)>,
}

impl Head {
    /// All values of header `name` (case-insensitive).
    pub fn all<'a, 'n>(&'a self, name: &'n str) -> impl Iterator<Item = &'a str> + use<'a, 'n> {
        self.headers.iter().filter(move |(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
    pub fn get(&self, name: &str) -> Option<&str> {
        self.all(name).next()
    }
}

fn is_tchar(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
}

/// Writes a request. `extra` headers are already validated by the caller.
pub fn write_request<W: Write>(
    w: &mut W,
    method: &str,
    target: &str,
    host: &str,
    headers: &[(&str, String)],
    body: &[u8],
) -> io::Result<()> {
    let mut r = format!("{method} {target} HTTP/1.1\r\nHost: {host}\r\n");
    for (k, v) in headers {
        debug_assert!(!v.contains('\r') && !v.contains('\n'));
        r.push_str(&format!("{k}: {v}\r\n"));
    }
    if method == "POST" {
        r.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    r.push_str("\r\n");
    let mut data = r.into_bytes();
    data.extend_from_slice(body);
    w.write_all(&data)?;
    w.flush()
}

/// Reads the status line and headers (skipping 100-199 interim responses).
pub fn read_head<S: Read>(c: &mut Conn<S>) -> Result<Head, HttpError> {
    for _ in 0..8 {
        let mut total = 0usize;
        let status_line = c.line(MAX_LINE)?;
        let s = std::str::from_utf8(&status_line).map_err(|_| HttpError::Malformed("the status line"))?;
        let rest = s.strip_prefix("HTTP/1.1 ").or_else(|| s.strip_prefix("HTTP/1.0 "));
        let rest = rest.ok_or(HttpError::Malformed("not an HTTP/1.x response"))?;
        let code = rest.get(..3).ok_or(HttpError::Malformed("the status code"))?;
        if !code.bytes().all(|b| b.is_ascii_digit()) || (rest.len() > 3 && !rest[3..].starts_with(' ')) {
            return Err(HttpError::Malformed("the status code"));
        }
        let status: u16 = code.parse().map_err(|_| HttpError::Malformed("the status code"))?;
        if !(100..=599).contains(&status) {
            return Err(HttpError::Malformed("the status code"));
        }
        let mut headers = Vec::new();
        loop {
            let line = c.line(MAX_LINE)?;
            total += line.len() + 2;
            if total > MAX_HEADER_BYTES || headers.len() > MAX_HEADERS {
                return Err(HttpError::TooLarge);
            }
            if line.is_empty() {
                break;
            }
            if line[0] == b' ' || line[0] == b'\t' {
                return Err(HttpError::Malformed("obsolete header line folding"));
            }
            let colon = line.iter().position(|&b| b == b':').ok_or(HttpError::Malformed("a header without a colon"))?;
            let (name, value) = (&line[..colon], &line[colon + 1..]);
            if name.is_empty() || !name.iter().all(|&b| is_tchar(b)) {
                return Err(HttpError::Malformed("a header name"));
            }
            if value.iter().any(|&b| b == 0 || b == b'\r' || b == b'\n') {
                return Err(HttpError::Malformed("a header value"));
            }
            let value = String::from_utf8_lossy(value).trim_matches(|c| c == ' ' || c == '\t').to_string();
            headers.push((String::from_utf8_lossy(name).into_owned(), value));
        }
        if status == 101 {
            return Err(HttpError::Malformed("an unexpected protocol switch"));
        }
        if (100..200).contains(&status) {
            continue; // interim response; the real one follows
        }
        return Ok(Head { status, headers });
    }
    Err(HttpError::Malformed("too many interim responses"))
}

fn parse_decimal(s: &str) -> Option<u64> {
    if s.is_empty() || s.len() > 18 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// How the body is delimited.
pub enum Framing {
    None,
    Length(u64),
    Chunked,
    Close,
}

pub fn framing(head: &Head, method_is_head: bool) -> Result<Framing, HttpError> {
    if method_is_head || head.status == 204 || head.status == 304 {
        return Ok(Framing::None);
    }
    let te: Vec<&str> = head.all("transfer-encoding").collect();
    let cl: Vec<&str> = head.all("content-length").collect();
    if !te.is_empty() {
        // Both is a request-smuggling signature: refuse.
        if !cl.is_empty() {
            return Err(HttpError::Malformed("both Transfer-Encoding and Content-Length"));
        }
        let codings: Vec<String> =
            te.iter().flat_map(|v| v.split(',')).map(|c| c.trim().to_ascii_lowercase()).filter(|c| !c.is_empty()).collect();
        if codings != ["chunked"] {
            return Err(HttpError::Malformed("an unsupported Transfer-Encoding"));
        }
        return Ok(Framing::Chunked);
    }
    if !cl.is_empty() {
        // Repeated values ("5, 5" or two headers) must all agree.
        let mut val = None;
        for v in cl.iter().flat_map(|v| v.split(',')) {
            let n = parse_decimal(v.trim()).ok_or(HttpError::Malformed("Content-Length"))?;
            if val.is_some_and(|x| x != n) {
                return Err(HttpError::Malformed("conflicting Content-Length values"));
            }
            val = Some(n);
        }
        return Ok(Framing::Length(val.unwrap()));
    }
    Ok(Framing::Close)
}

/// Reads the body. `max` bounds what is kept (the encoded size).
pub fn read_body<S: Read>(c: &mut Conn<S>, f: Framing, max: usize) -> Result<Vec<u8>, HttpError> {
    let mut out = Vec::new();
    match f {
        Framing::None => {}
        Framing::Length(n) => {
            if n > max as u64 {
                return Err(HttpError::TooLarge);
            }
            c.read_exact_into(n as usize, &mut out)?;
        }
        Framing::Chunked => {
            loop {
                let line = c.line(1024)?;
                let s = std::str::from_utf8(&line).map_err(|_| HttpError::Malformed("a chunk size"))?;
                let size = s.split(';').next().unwrap_or("").trim_end_matches([' ', '\t']);
                if size.is_empty() || size.len() > 15 || !size.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err(HttpError::Malformed("a chunk size"));
                }
                let n = u64::from_str_radix(size, 16).map_err(|_| HttpError::Malformed("a chunk size"))?;
                if n == 0 {
                    // Trailers, then an empty line.
                    let mut total = 0;
                    loop {
                        let t = c.line(MAX_LINE)?;
                        total += t.len() + 2;
                        if total > MAX_HEADER_BYTES {
                            return Err(HttpError::TooLarge);
                        }
                        if t.is_empty() {
                            break;
                        }
                    }
                    break;
                }
                if out.len() as u64 + n > max as u64 {
                    return Err(HttpError::TooLarge);
                }
                c.read_exact_into(n as usize, &mut out)?;
                if !c.line(1)?.is_empty() {
                    return Err(HttpError::Malformed("chunk data longer than its size"));
                }
            }
        }
        Framing::Close => loop {
            let s = c.read_some(64 * 1024)?;
            if s.is_empty() {
                break;
            }
            if out.len() + s.len() > max {
                return Err(HttpError::TooLarge);
            }
            out.extend_from_slice(s);
        },
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn conn(s: &'static [u8]) -> Conn<&'static [u8]> {
        Conn::new(s, Instant::now() + Duration::from_secs(5))
    }

    fn get(raw: &'static [u8]) -> Result<(u16, Vec<u8>), HttpError> {
        let mut c = conn(raw);
        let h = read_head(&mut c)?;
        let f = framing(&h, false)?;
        Ok((h.status, read_body(&mut c, f, 1000)?))
    }

    #[test]
    fn content_length() {
        let (s, b) = get(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhelloEXTRA").unwrap();
        assert_eq!((s, &b[..]), (200, &b"hello"[..]));
        assert!(matches!(get(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nhello"), Err(HttpError::Truncated)));
        assert!(get(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nContent-Length: 6\r\n\r\nhello!").is_err());
        assert!(get(b"HTTP/1.1 200 OK\r\nContent-Length: 5, 5\r\n\r\nhello").is_ok());
        assert!(get(b"HTTP/1.1 200 OK\r\nContent-Length: -1\r\n\r\n").is_err());
        assert!(get(b"HTTP/1.1 200 OK\r\nContent-Length: +5\r\n\r\nhello").is_err());
        assert!(matches!(get(b"HTTP/1.1 200 OK\r\nContent-Length: 5000\r\n\r\n"), Err(HttpError::TooLarge)));
    }

    #[test]
    fn chunked() {
        let (_, b) = get(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5;x=y\r\nhello\r\n6\r\n world\r\n0\r\nX-T: 1\r\n\r\n").unwrap();
        assert_eq!(b, b"hello world");
        assert!(get(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhelloXX\r\n0\r\n\r\n").is_err());
        assert!(get(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n").is_err());
        assert!(get(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip, chunked\r\n\r\n0\r\n\r\n").is_err());
        assert!(get(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Length: 3\r\n\r\n0\r\n\r\n").is_err());
        assert!(matches!(
            get(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nfffffffffff\r\n"),
            Err(HttpError::TooLarge)
        ));
    }

    #[test]
    fn heads() {
        let (s, b) = get(b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 404 Not Found\r\n\r\nbody").unwrap();
        assert_eq!((s, &b[..]), (404, &b"body"[..]));
        assert!(get(b"HTTP/1.1 200 OK\r\nX: a\r\n folded\r\n\r\n").is_err());
        assert!(get(b"HTTP/1.1 200 OK\r\nBad Name: a\r\n\r\n").is_err());
        assert!(get(b"HTTP/1.1 200 OK\r\nX: a\rb\r\n\r\n").is_err());
        assert!(get(b"HTTP/2 200\r\n\r\n").is_err());
        assert!(get(b"HTTP/1.1 2000 OK\r\n\r\n").is_err());
        assert!(get(b"HTTP/1.1 200 OK\nX: lf only\n\nok").is_ok());
        let mut c = conn(b"HTTP/1.1 204 No Content\r\nA: 1\r\na: 2\r\n\r\n");
        let h = read_head(&mut c).unwrap();
        assert_eq!(h.all("A").collect::<Vec<_>>(), ["1", "2"]);
        assert!(matches!(framing(&h, false).unwrap(), Framing::None));
    }
}
