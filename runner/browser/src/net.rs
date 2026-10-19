//! net.rs - the seam between the browser and æther's network layer.
//!
//! Everything the browser knows about the network comes through these
//! re-exports from the `webnet` crate (runner/webnet): URLs, HTTP/1.1 and
//! TLS 1.3, with the kernel resolving names and its firewall deciding every
//! connection. Tests swap the fetch function for `testserve::fetch`.

pub use webnet::{fetch, FetchError, Method, Request, Response, Url, UrlError};

/// The value of a response header (names compare case-insensitively).
pub fn header<'a>(r: &'a Response, name: &str) -> Option<&'a str> {
    r.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
}
