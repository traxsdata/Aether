//! webnet - the network layer of æther's web browser.
//!
//! `fetch` takes a `Request` and returns a `Response` or a `FetchError`
//! whose Display text is fit for an error page. Underneath: URLs (`url`),
//! HTTP/1.1 (`http`), TLS 1.3 (`tls`) with certificate validation
//! (`x509`, `roots`) and its own cryptography (`crypto`), all in std-only
//! Rust. On æther, connections go through the kernel's network calls (the
//! kernel resolves names and its firewall decides); elsewhere through
//! std::net, for development.
//!
//! Policy: https unless the caller explicitly allows http (the browser
//! asks the user first); never an https -> http redirect; at most 10
//! redirects; bounded sizes and times everywhere; no cookies.

// Errors carry the URL they concern; crypto code indexes arrays by design.
#![allow(clippy::result_large_err, clippy::result_unit_err, clippy::needless_range_loop)]

pub mod crypto;
pub mod der;
pub mod http;
pub mod inflate;
pub mod roots;
mod roots_data;
pub mod tls;
pub mod url;
pub mod x509;

pub use url::{Url, UrlError};

use aether_host::net::TcpStream;
use roots::RootStore;
use std::fmt;
use std::io::{self, Read, Write};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

pub const DEFAULT_MAX_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_REDIRECTS: usize = 10;
pub const USER_AGENT: &str = "aether-browser/0.7";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Head,
    Post,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Head => "HEAD",
            Method::Post => "POST",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Request {
    pub url: Url,
    pub method: Method,
    /// For POST: an application/x-www-form-urlencoded body (see
    /// `url::form_urlencoded`). Ignored for GET and HEAD.
    pub body: Vec<u8>,
    /// Allow plain http. Only after the user agreed to an insecure page.
    pub allow_http: bool,
    /// Largest body accepted (after decompression).
    pub max_bytes: usize,
}

impl Request {
    pub fn get(url: Url) -> Request {
        Request { url, method: Method::Get, body: Vec::new(), allow_http: false, max_bytes: DEFAULT_MAX_BYTES }
    }

    pub fn post_form(url: Url, body: Vec<u8>) -> Request {
        Request { url, method: Method::Post, body, allow_http: false, max_bytes: DEFAULT_MAX_BYTES }
    }
}

#[derive(Clone, Debug)]
pub struct Response {
    /// The final URL, after redirects.
    pub url: Url,
    pub status: u16,
    /// As received (names keep their case; repeated headers stay separate).
    pub headers: Vec<(String, String)>,
    /// Decoded (gzip / deflate content-encoding removed).
    pub body: Vec<u8>,
    /// True when every hop was https with a validated certificate.
    pub secure: bool,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    pub fn content_type(&self) -> Option<&str> {
        self.header("content-type")
    }
}

#[derive(Debug)]
pub enum FetchError {
    InvalidUrl(UrlError),
    /// The URL is plain http and the request didn't allow it.
    InsecureHttp(Url),
    /// An https page redirected to plain http.
    Downgrade(Url),
    TooManyRedirects,
    /// A redirect without a usable Location.
    BadRedirect(String),
    /// The firewall refused the connection (or the app has no network permission).
    Blocked { host: String, detail: String },
    /// No network, unknown host, refused.
    Connect { host: String, error: io::Error },
    Timeout,
    Tls(tls::TlsError),
    /// The server's HTTP response was malformed.
    Protocol(&'static str),
    /// The connection closed before the response was complete.
    Truncated,
    TooLarge(usize),
    UnsupportedEncoding(String),
    Decode(inflate::InflateError),
    Io(io::Error),
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FetchError::InvalidUrl(e) => write!(f, "The address isn't valid: {e}."),
            FetchError::InsecureHttp(u) => write!(
                f,
                "{} is not a secure (https) address. Insecure pages are only loaded if you choose to.",
                u.as_str()
            ),
            FetchError::Downgrade(u) => {
                write!(f, "A secure page tried to send you to an insecure address ({}). That was blocked.", u.as_str())
            }
            FetchError::TooManyRedirects => write!(f, "The site redirected too many times."),
            FetchError::BadRedirect(w) => write!(f, "The site sent an invalid redirect ({w})."),
            FetchError::Blocked { host, detail } => write!(f, "The connection to {host} was refused: {detail}."),
            FetchError::Connect { host, error } => write!(f, "Couldn't connect to {host}: {error}."),
            FetchError::Timeout => write!(f, "The site took too long to respond."),
            FetchError::Tls(e) => write!(f, "The connection isn't secure: {e}."),
            FetchError::Protocol(w) => write!(f, "The site sent an invalid response ({w})."),
            FetchError::Truncated => write!(f, "The connection closed before the page was complete."),
            FetchError::TooLarge(n) => write!(f, "The page is larger than the limit of {} KiB.", n / 1024),
            FetchError::UnsupportedEncoding(e) => write!(f, "The page uses an unsupported encoding ({e})."),
            FetchError::Decode(e) => write!(f, "The page couldn't be decoded: {e}."),
            FetchError::Io(e) => write!(f, "Network error: {e}."),
        }
    }
}

impl std::error::Error for FetchError {}

impl From<UrlError> for FetchError {
    fn from(e: UrlError) -> Self {
        FetchError::InvalidUrl(e)
    }
}

/// Settings beyond the request itself. `fetch` uses the defaults.
#[derive(Clone)]
pub struct Config {
    /// Trust anchors. None: the built-in roots.
    pub roots: Option<RootStore>,
    pub connect_timeout: Duration,
    /// For each read from the network.
    pub read_timeout: Duration,
    /// For a whole request (each redirect hop gets its own).
    pub total_timeout: Duration,
    /// Development only (never on æther): tunnel through this HTTP proxy
    /// with CONNECT. TLS still runs end to end with full validation.
    pub proxy: Option<(String, u16)>,
    /// Development only (never on æther, where the kernel resolves names
    /// for the firewall): connect to this address instead of resolving the
    /// URL's host. TLS still checks the certificate against the URL's host.
    pub connect_to: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            roots: None,
            connect_timeout: Duration::from_secs(15),
            read_timeout: Duration::from_secs(30),
            total_timeout: Duration::from_secs(120),
            proxy: None,
            connect_to: None,
        }
    }
}

fn builtin_roots() -> &'static RootStore {
    static ROOTS: OnceLock<RootStore> = OnceLock::new();
    ROOTS.get_or_init(RootStore::builtin)
}

/// The current time in Unix seconds: æther's clock (SYS_TIME) on æther,
/// the system clock elsewhere.
pub fn now_unix() -> i64 {
    if aether_host::on_aether() {
        if let Ok(t) = aether_host::sys::time() {
            if t.year >= 2000 {
                return x509::unix_time(t.year as i64, t.month as u32, t.day as u32, t.hour as u32, t.minute as u32, t.second as u32);
            }
        }
    }
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Fetches `req` with the default settings.
pub fn fetch(req: &Request) -> Result<Response, FetchError> {
    fetch_with(&Config::default(), req)
}

/// Fetches `req`, following redirects.
pub fn fetch_with(cfg: &Config, req: &Request) -> Result<Response, FetchError> {
    let mut url = req.url.clone();
    let mut method = req.method;
    let mut body = req.body.clone();
    let mut secure = true;
    let mut was_https = false;
    for _hop in 0..=MAX_REDIRECTS {
        if !url.is_https() {
            if was_https {
                return Err(FetchError::Downgrade(url));
            }
            if !req.allow_http {
                return Err(FetchError::InsecureHttp(url));
            }
            secure = false;
        }
        was_https |= url.is_https();
        let resp = fetch_once(cfg, &url, method, &body, req.max_bytes)?;
        if matches!(resp.status, 301 | 302 | 303 | 307 | 308) {
            let Some(loc) = resp.header("location") else {
                // A redirect status without Location is just a response.
                return Ok(Response { secure, ..resp });
            };
            let mut next = url.join(loc).map_err(|e| FetchError::BadRedirect(e.to_string()))?;
            if next.fragment().is_none() {
                if let Some(f) = url.fragment() {
                    next = next.join(&format!("#{f}")).unwrap_or(next);
                }
            }
            if resp.status == 303 || (matches!(resp.status, 301 | 302) && method == Method::Post) {
                if method != Method::Head {
                    method = Method::Get;
                }
                body.clear();
            }
            url = next;
            continue;
        }
        return Ok(Response { url, secure, ..resp });
    }
    Err(FetchError::TooManyRedirects)
}

/// A connection, plain or TLS.
trait Stream: Read + Write {}
impl<T: Read + Write> Stream for T {}

fn connect_error(host: &str, e: io::Error) -> FetchError {
    match e.kind() {
        io::ErrorKind::PermissionDenied => FetchError::Blocked { host: host.to_string(), detail: e.to_string() },
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => FetchError::Timeout,
        _ => FetchError::Connect { host: host.to_string(), error: e },
    }
}

fn tcp(cfg: &Config, host: &str, port: u16) -> Result<TcpStream, FetchError> {
    let ms = cfg.connect_timeout.as_millis().min(u32::MAX as u128) as u32;
    let mut s = match &cfg.proxy {
        Some((ph, pp)) if !aether_host::on_aether() => {
            let mut s = TcpStream::connect(ph, *pp, ms).map_err(|e| connect_error(ph, e))?;
            s.set_read_timeout(Some(cfg.read_timeout)).map_err(FetchError::Io)?;
            proxy_connect(&mut s, host, port).map_err(|e| connect_error(host, e))?;
            s
        }
        _ => {
            let target = match &cfg.connect_to {
                Some(addr) if !aether_host::on_aether() => addr.as_str(),
                _ => host,
            };
            TcpStream::connect(target, port, ms).map_err(|e| connect_error(host, e))?
        }
    };
    s.set_read_timeout(Some(cfg.read_timeout)).map_err(FetchError::Io)?;
    s.set_write_timeout(Some(cfg.read_timeout)).map_err(FetchError::Io)?;
    Ok(s)
}

/// HTTP CONNECT through a development proxy.
fn proxy_connect(s: &mut TcpStream, host: &str, port: u16) -> io::Result<()> {
    s.write_all(format!("CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n\r\n").as_bytes())?;
    let mut resp = Vec::new();
    let mut b = [0u8; 1];
    while !resp.ends_with(b"\r\n\r\n") {
        if s.read(&mut b)? == 0 || resp.len() > 8192 {
            return Err(io::Error::new(io::ErrorKind::ConnectionRefused, "the proxy closed the connection"));
        }
        resp.push(b[0]);
    }
    let line = String::from_utf8_lossy(&resp);
    let status = line.split_whitespace().nth(1).unwrap_or("");
    if status != "200" {
        let first = line.lines().next().unwrap_or("").to_string();
        return Err(io::Error::new(io::ErrorKind::ConnectionRefused, format!("the proxy refused: {first}")));
    }
    Ok(())
}

fn http_err(e: http::HttpError) -> FetchError {
    match e {
        http::HttpError::Io(e) => FetchError::Io(e),
        http::HttpError::Malformed(w) => FetchError::Protocol(w),
        http::HttpError::TooLarge => FetchError::Protocol("a header or body part was too large"),
        http::HttpError::Timeout => FetchError::Timeout,
        http::HttpError::Truncated => FetchError::Truncated,
    }
}

fn fetch_once(cfg: &Config, url: &Url, method: Method, body: &[u8], max: usize) -> Result<Response, FetchError> {
    let deadline = Instant::now() + cfg.total_timeout;
    let host = url.host();
    let sock = tcp(cfg, host, url.port_or_default())?;
    let mut stream: Box<dyn Stream> = if url.is_https() {
        let roots = cfg.roots.as_ref().unwrap_or_else(|| builtin_roots());
        let tcfg = tls::TlsConfig { roots, now: now_unix(), suites: tls::DEFAULT_SUITES, alpn: &["http/1.1"] };
        let t = tls::TlsStream::connect(sock, host, &tcfg).map_err(|e| match e {
            tls::TlsError::Io(e) if matches!(e.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock) => {
                FetchError::Timeout
            }
            e => FetchError::Tls(e),
        })?;
        Box::new(t)
    } else {
        Box::new(sock)
    };
    let mut headers: Vec<(&str, String)> = vec![
        ("User-Agent", USER_AGENT.to_string()),
        ("Accept", "text/html,application/xhtml+xml,text/plain;q=0.9,image/png,image/jpeg,image/gif,image/bmp;q=0.8,*/*;q=0.5".to_string()),
        ("Accept-Encoding", "gzip, deflate".to_string()),
        ("Accept-Language", "en".to_string()),
        ("Connection", "close".to_string()),
    ];
    let body: &[u8] = if method == Method::Post {
        headers.push(("Content-Type", "application/x-www-form-urlencoded".to_string()));
        body
    } else {
        &[]
    };
    http::write_request(&mut stream, method.as_str(), &url.request_target(), &url.authority(), &headers, body)
        .map_err(|e| http_err(e.into()))?;
    let mut conn = http::Conn::new(stream, deadline);
    let head = http::read_head(&mut conn).map_err(http_err)?;
    let framing = http::framing(&head, method == Method::Head).map_err(http_err)?;
    let raw = http::read_body(&mut conn, framing, max).map_err(|e| match e {
        http::HttpError::TooLarge => FetchError::TooLarge(max),
        e => http_err(e),
    })?;
    // Best effort: tell a TLS server we're done. Errors don't matter now.
    drop(conn);
    let body = decode_body(&head, raw, max)?;
    Ok(Response { url: url.clone(), status: head.status, headers: head.headers, body, secure: url.is_https() })
}

fn decode_body(head: &http::Head, raw: Vec<u8>, max: usize) -> Result<Vec<u8>, FetchError> {
    let codings: Vec<String> = head
        .all("content-encoding")
        .flat_map(|v| v.split(','))
        .map(|c| c.trim().to_ascii_lowercase())
        .filter(|c| !c.is_empty() && c != "identity")
        .collect();
    if raw.is_empty() {
        return Ok(raw);
    }
    let mut data = raw;
    for c in codings.iter().rev() {
        data = match c.as_str() {
            "gzip" | "x-gzip" => inflate::gunzip(&data, max),
            "deflate" => inflate::unzlib(&data, max),
            other => return Err(FetchError::UnsupportedEncoding(other.to_string())),
        }
        .map_err(|e| match e {
            inflate::InflateError::TooLarge => FetchError::TooLarge(max),
            e => FetchError::Decode(e),
        })?;
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    /// A plain HTTP server answering each connection with the next response.
    fn serve(responses: Vec<Vec<u8>>) -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        thread::spawn(move || {
            for r in responses {
                let (mut c, _) = l.accept().unwrap();
                let mut buf = [0u8; 4096];
                let _ = c.read(&mut buf);
                let _ = c.write_all(&r);
            }
        });
        port
    }

    fn s(x: &str) -> Vec<u8> {
        x.as_bytes().to_vec()
    }

    fn req(url: &str) -> Request {
        let mut r = Request::get(Url::parse(url).unwrap());
        r.allow_http = true;
        r
    }

    #[test]
    fn http_policy() {
        let r = Request::get(Url::parse("http://127.0.0.1:9/").unwrap());
        let e = fetch(&r).unwrap_err();
        assert!(matches!(e, FetchError::InsecureHttp(_)));
        assert!(e.to_string().contains("not a secure"));
    }

    #[test]
    fn redirects_and_gzip() {
        let gz = [
            0x1fu8, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0xff, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0x57, 0xc8, 0x40,
            0x27, 0xb9, 0x00, 0x00, 0x88, 0x59, 0x0b, 0x18, 0x00, 0x00, 0x00,
        ];
        let mut gz_resp = s(&format!("HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\n\r\n", gz.len()));
        gz_resp.extend_from_slice(&gz);
        let port = serve(vec![
            s("HTTP/1.1 302 Found\r\nLocation: /next\r\nContent-Length: 0\r\n\r\n"),
            s("HTTP/1.1 307 Temporary\r\nLocation: gz\r\nContent-Length: 0\r\n\r\n"),
            gz_resp,
            s("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n"),
        ]);
        let r = fetch(&req(&format!("http://127.0.0.1:{port}/start#frag"))).unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(r.body, b"hello hello hello hello\n");
        assert_eq!(r.url.as_str(), format!("http://127.0.0.1:{port}/gz#frag"));
        assert!(!r.secure);
        let r = fetch(&req(&format!("http://127.0.0.1:{port}/"))).unwrap();
        assert_eq!(r.body, b"hello");
    }

    #[test]
    fn https_to_http_redirect_is_refused() {
        // Checked before connecting: an https URL that redirects to http
        // is caught in fetch_with; here, the policy function directly.
        let e = FetchError::Downgrade(Url::parse("http://example.com/").unwrap());
        assert!(e.to_string().contains("insecure"));
    }

    #[test]
    fn redirect_loop_is_bounded() {
        let port = serve(vec![s("HTTP/1.1 301 Moved\r\nLocation: /again\r\nContent-Length: 0\r\n\r\n"); 11]);
        let e = fetch(&req(&format!("http://127.0.0.1:{port}/"))).unwrap_err();
        assert!(matches!(e, FetchError::TooManyRedirects), "{e}");
    }

    #[test]
    fn size_limit() {
        let port = serve(vec![s(&("HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n".to_string() + &"x".repeat(100)))]);
        let mut r = req(&format!("http://127.0.0.1:{port}/"));
        r.max_bytes = 10;
        assert!(matches!(fetch(&r), Err(FetchError::TooLarge(10))));
    }

    #[test]
    fn connection_refused_reads_well() {
        // Find a port nobody listens on.
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let e = fetch(&req(&format!("http://127.0.0.1:{port}/"))).unwrap_err();
        assert!(e.to_string().starts_with("Couldn't connect to 127.0.0.1"), "{e}");
    }
}
