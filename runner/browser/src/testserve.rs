//! testserve.rs - a stand-in for the network in tests and headless runs on
//! a host: `https://test.aether/<path>` is answered from a local folder,
//! every other address is refused. Never used on æther.

use std::path::PathBuf;
use std::sync::OnceLock;

use crate::net::{FetchError, Request, Response};

static ROOT: OnceLock<PathBuf> = OnceLock::new();

/// Serves https://test.aether/ from `root` (the first call wins).
pub fn set_root(root: PathBuf) {
    let _ = ROOT.set(root);
}

/// The fetcher: a `page::Fetcher`.
pub fn fetch(req: &Request) -> Result<Response, FetchError> {
    let host = req.url.host().to_string();
    if host != "test.aether" {
        return Err(FetchError::Blocked { host, detail: "only test.aether is served in this test run".into() });
    }
    if req.url.scheme() == "http" && !req.allow_http {
        return Err(FetchError::InsecureHttp(req.url.clone()));
    }
    let path = req.url.path().to_string();
    let secure = req.url.scheme() == "https";
    let url = req.url.clone();
    match file(&path) {
        Some((ctype, body)) => {
            if body.len() > req.max_bytes {
                return Err(FetchError::TooLarge(req.max_bytes));
            }
            Ok(Response { url, status: 200, headers: vec![("Content-Type".into(), ctype.into())], body, secure })
        }
        None => Ok(Response {
            url,
            status: 404,
            headers: vec![("Content-Type".into(), "text/html".into())],
            body: b"<!doctype html><title>Not found</title><h1>404 Not found</h1><p>There is no such page on test.aether.".to_vec(),
            secure,
        }),
    }
}

fn file(path: &str) -> Option<(&'static str, Vec<u8>)> {
    let root = ROOT.get()?;
    let rel = path.trim_start_matches('/');
    let rel = if rel.is_empty() { "index.html" } else { rel };
    if rel.split('/').any(|s| s.is_empty() || s == ".." || s.starts_with('.')) || rel.contains(['%', '\\']) {
        return None;
    }
    let p = root.join(rel);
    let body = std::fs::read(&p).ok()?;
    let ctype = match p.extension().and_then(|e| e.to_str()) {
        Some("html") | Some("htm") => "text/html",
        Some("css") => "text/css",
        Some("txt") => "text/plain",
        Some("js") => "text/javascript",
        Some("json") => "application/json",
        Some("png") => "image/png",
        _ => "application/octet-stream",
    };
    Some((ctype, body))
}
