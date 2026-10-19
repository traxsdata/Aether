//! page.rs - loading a page: fetch, decode, parse, gather stylesheets
//! (inline and linked, with @import), and lay it out for a viewport.
//!
//! Scripts are fetched here too (when the site may run them), but nothing
//! runs on this thread: the window runs them once the page is in (see
//! script::Runtime).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::css::{compute_styles, MediaEnv, Stylesheet};
use crate::dom::{Document, NodeData, NodeId};
use crate::html::{self, Charset};
use crate::image::{self, Image};
use crate::layout::{apply_image_sizes, build_tree, layout_document, Page};
use crate::net::{self, FetchError, Method, Request, Response, Url};
use crate::paint::FormState;
use crate::script::{Mode, PageScript, Policy, Source};

/// Limits on what a page may pull in.
pub const MAX_PAGE_BYTES: usize = 8 << 20;
pub const MAX_SHEET_BYTES: usize = 2 << 20;
pub const MAX_SHEETS: usize = 48;
pub const MAX_IMPORT_DEPTH: usize = 4;
pub const MAX_INLINE_CSS: usize = 4 << 20;
pub const MAX_SCRIPTS: usize = 200;
pub const MAX_SCRIPT_BYTES: usize = 4 << 20;
pub const MAX_ALL_SCRIPT_BYTES: usize = 24 << 20;
pub const MAX_IMAGES: usize = 96;
pub const MAX_IMAGE_BYTES: usize = 8 << 20;
pub const MAX_ALL_IMAGE_BYTES: usize = 32 << 20;
pub const MAX_ALL_IMAGE_PIXELS: u64 = 32 * 1024 * 1024;

/// Where a page came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Location {
    /// A built-in page (about:home, about:security).
    About(String),
    Web(Url),
}

impl Location {
    pub fn display(&self) -> String {
        match self {
            Location::About(s) => format!("about:{s}"),
            Location::Web(u) => u.as_str().to_string(),
        }
    }
}

#[derive(Debug)]
pub enum LoadError {
    Fetch(FetchError),
    /// The address couldn't be understood.
    BadAddress(String),
    /// The response isn't something the browser displays.
    Unsupported(String),
    Cancelled,
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Fetch(e) => write!(f, "{e}"),
            LoadError::BadAddress(s) => write!(f, "{s}"),
            LoadError::Unsupported(s) => write!(f, "{s}"),
            LoadError::Cancelled => write!(f, "loading was stopped"),
        }
    }
}

/// A fetched and parsed page, before layout (it is `Send`).
pub struct Loaded {
    pub location: Location,
    pub doc: Document,
    pub sheets: Vec<Stylesheet>,
    /// Whether the page and everything it pulled in came over https (or is built in).
    pub secure: bool,
    pub status: u16,
    pub charset: Charset,
    /// What relative links resolve against (the URL, or <base href>).
    pub base: Option<Url>,
    pub title: String,
    /// Fragment to scroll to after layout.
    pub fragment: Option<String>,
    /// Subresources that were refused or failed (for the status line).
    pub blocked: Vec<String>,
    /// Decoded pictures, keyed by their `<img>` node.
    pub images: HashMap<NodeId, Arc<Image>>,
    pub allow_http: bool,
    /// What scripts this page may run, and the ones it has, in run order.
    pub script_mode: Mode,
    pub scripts: Vec<PageScript>,
}

/// How the loader reaches the network (tests substitute their own).
pub type Fetcher<'a> = &'a (dyn Fn(&Request) -> Result<Response, FetchError> + Sync);

pub fn default_fetcher() -> Fetcher<'static> {
    &net::fetch
}

/// Fetches and parses `req`'s page, its stylesheets and (as `policy`
/// allows for the site it ends up on) its scripts.
pub fn load(req: Request, fragment: Option<String>, fetch: Fetcher, cancel: &AtomicBool, policy: &Policy) -> Result<Loaded, LoadError> {
    let allow_http = req.allow_http;
    let mut req = req;
    req.max_bytes = req.max_bytes.min(MAX_PAGE_BYTES);
    let resp = fetch(&req).map_err(LoadError::Fetch)?;
    if cancel.load(Ordering::Relaxed) {
        return Err(LoadError::Cancelled);
    }
    let ctype = net::header(&resp, "content-type").unwrap_or("text/html").to_string();
    let mime = ctype.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    let (doc, charset) = match mime.as_str() {
        "text/html" | "application/xhtml+xml" | "" => html::parse_bytes(&resp.body, Some(&ctype)),
        "text/plain" | "text/css" | "text/csv" | "text/markdown" | "application/json" | "text/xml" | "application/xml" => {
            let (text, cs) = html::decode(&resp.body, Some(&ctype));
            (plain_text_document(&text), cs)
        }
        m if m.starts_with("image/") => {
            return Err(LoadError::Unsupported(format!("This is an image ({m}). The browser shows pages, not image files, and doesn't download files.")));
        }
        m => return Err(LoadError::Unsupported(format!("This address returned a file of type {m}, which the browser doesn't display. It doesn't download files either."))),
    };
    let mut loaded = finish(Location::Web(resp.url.clone()), doc, charset, resp.secure, resp.status, fragment, Some(fetch), cancel, allow_http)?;
    loaded.secure &= resp.secure;
    let html = matches!(mime.as_str(), "text/html" | "application/xhtml+xml" | "");
    if html {
        gather_images(&mut loaded, fetch, cancel)?;
        loaded.script_mode = policy.mode_for(&resp.url);
        if loaded.script_mode.runs() {
            gather_scripts(&mut loaded, fetch, cancel)?;
        }
    }
    Ok(loaded)
}

/// Parses a built-in page.
pub fn load_builtin(name: &str, html_src: &str) -> Loaded {
    let doc = html::parse(html_src);
    let never = AtomicBool::new(false);
    match finish(Location::About(name.to_string()), doc, Charset::Utf8, true, 200, None, None, &never, false) {
        Ok(l) => l,
        Err(_) => unreachable!("built-in pages fetch nothing"),
    }
}

fn plain_text_document(text: &str) -> Document {
    let mut s = String::with_capacity(text.len() + 64);
    s.push_str("<!doctype html><pre style=\"white-space:pre-wrap\">");
    for c in text.chars() {
        match c {
            '<' => s.push_str("&lt;"),
            '&' => s.push_str("&amp;"),
            c => s.push(c),
        }
    }
    s.push_str("</pre>");
    html::parse(&s)
}

#[allow(clippy::too_many_arguments)]
fn finish(
    location: Location,
    doc: Document,
    charset: Charset,
    secure: bool,
    status: u16,
    fragment: Option<String>,
    fetch: Option<Fetcher>,
    cancel: &AtomicBool,
    allow_http: bool,
) -> Result<Loaded, LoadError> {
    let page_url = match &location {
        Location::Web(u) => Some(u.clone()),
        Location::About(_) => None,
    };
    // <base href> (the first one with an href).
    let base = page_url.as_ref().map(|u| {
        doc.find_all("base")
            .into_iter()
            .find_map(|b| doc.element(b).and_then(|e| e.attr("href")))
            .and_then(|h| u.join(h.trim()).ok())
            .unwrap_or_else(|| u.clone())
    });
    let mut l = Loaded {
        title: doc.title(),
        location,
        doc,
        sheets: Vec::new(),
        secure,
        status,
        charset,
        base,
        fragment,
        blocked: Vec::new(),
        images: HashMap::new(),
        allow_http,
        script_mode: Mode::Off,
        scripts: Vec::new(),
    };
    gather_styles(&mut l, fetch, cancel)?;
    Ok(l)
}

/// The page's stylesheets in document order: <style> elements and
/// <link rel=stylesheet>, each followed by what it @imports.
fn gather_styles(l: &mut Loaded, fetch: Option<Fetcher>, cancel: &AtomicBool) -> Result<(), LoadError> {
    let env = MediaEnv::default();
    let mut inline_bytes = 0usize;
    let nodes: Vec<NodeId> = match l.doc.html_element() {
        Some(h) => l.doc.descendants(h),
        None => Vec::new(),
    };
    for n in nodes {
        if l.sheets.len() >= MAX_SHEETS {
            break;
        }
        let Some(e) = l.doc.element(n) else { continue };
        if e.foreign {
            continue;
        }
        let media_ok = e.attr("media").map(|m| media_ok(m, &env)).unwrap_or(true);
        if !media_ok {
            continue;
        }
        match e.name.as_str() {
            "style" => {
                let ty = e.attr("type").unwrap_or("text/css").trim().to_ascii_lowercase();
                if !(ty.is_empty() || ty == "text/css") {
                    continue;
                }
                let text: String = l.doc.children(n).iter().filter_map(|&c| match &l.doc.nodes[c].data {
                    NodeData::Text(t) => Some(t.as_str()),
                    _ => None,
                }).collect();
                inline_bytes += text.len();
                if inline_bytes > MAX_INLINE_CSS {
                    continue;
                }
                let sheet = Stylesheet::parse(&text, &env);
                let base = l.base.clone();
                push_sheet(l, sheet, base.as_ref(), fetch, cancel, 0)?;
            }
            "link" => {
                let rel = e.attr("rel").unwrap_or("").to_ascii_lowercase();
                let words: Vec<&str> = rel.split_ascii_whitespace().collect();
                if !words.contains(&"stylesheet") || words.contains(&"alternate") {
                    continue;
                }
                let Some(href) = e.attr("href").map(|h| h.trim().to_string()) else { continue };
                if let (Some(base), Some(f)) = (l.base.clone(), fetch) {
                    if let Some((sheet, url)) = fetch_sheet(l, &base, &href, f, cancel)? {
                        push_sheet(l, sheet, Some(&url), fetch, cancel, 0)?;
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// The page's pictures: `<img src>` (or the largest `srcset` candidate),
/// fetched over https and decoded with the browser's own decoders. Data URLs
/// are decoded without touching the network. Limits keep a hostile gallery
/// from turning into a memory problem; a picture that fails is left as its
/// alt-text placeholder.
fn gather_images(l: &mut Loaded, fetch: Fetcher, cancel: &AtomicBool) -> Result<(), LoadError> {
    let (Some(base), Location::Web(page)) = (l.base.clone(), l.location.clone()) else { return Ok(()) };
    let mut wanted: Vec<(NodeId, Url)> = Vec::new();
    let mut inline: Vec<(NodeId, Vec<u8>, String)> = Vec::new();
    for n in l.doc.find_all("img") {
        if wanted.len() + inline.len() >= MAX_IMAGES {
            l.blocked.push("too many pictures; the rest were not loaded".into());
            break;
        }
        let Some(e) = l.doc.element(n) else { continue };
        if e.foreign {
            continue;
        }
        let src = e
            .attr("src")
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| first_srcset(e.attr("srcset")));
        let Some(src) = src else { continue };
        if let Some((bytes, mime)) = data_url(&src) {
            inline.push((n, bytes, mime));
            continue;
        }
        let Ok(u) = base.join(&src) else {
            l.blocked.push(format!("{src}: bad picture address"));
            continue;
        };
        if u.scheme() != "http" && u.scheme() != "https" {
            l.blocked.push(format!("{}: pictures load only over http(s)", u.as_str()));
            continue;
        }
        if u.scheme() == "http" && page.scheme() == "https" {
            l.blocked.push(format!("{}: a secure page can't load a plain-http picture", u.as_str()));
            continue;
        }
        wanted.push((n, u));
    }
    let mut total_bytes = 0usize;
    let mut total_pixels = 0u64;
    'batches: for batch in wanted.chunks(4) {
        if cancel.load(Ordering::Relaxed) {
            return Err(LoadError::Cancelled);
        }
        if total_bytes >= MAX_ALL_IMAGE_BYTES {
            l.blocked.push("the page's pictures are too big; the rest were not loaded".into());
            break;
        }
        let results: Vec<Result<Response, FetchError>> = std::thread::scope(|sc| {
            let handles: Vec<_> = batch
                .iter()
                .map(|(_, u)| {
                    let mut req = Request::get(u.clone());
                    req.max_bytes = MAX_IMAGE_BYTES;
                    req.allow_http = l.allow_http && u.scheme() == "http";
                    std::thread::Builder::new().stack_size(2 << 20).spawn_scoped(sc, move || fetch(&req))
                })
                .collect();
            handles
                .into_iter()
                .map(|h| match h {
                    Ok(h) => h.join().unwrap_or(Err(FetchError::Timeout)),
                    Err(_) => Err(FetchError::Timeout),
                })
                .collect()
        });
        for ((n, u), r) in batch.iter().zip(results) {
            let img = match r {
                Ok(r) if (200..300).contains(&r.status) => {
                    if !r.secure {
                        l.secure = false;
                    }
                    total_bytes += r.body.len();
                    if total_bytes > MAX_ALL_IMAGE_BYTES {
                        l.blocked.push("the page's pictures are too big; the rest were not loaded".into());
                        break 'batches;
                    }
                    let mime = r.content_type().map(str::to_string);
                    match image::decode(&r.body, mime.as_deref()) {
                        Ok(img) => img,
                        Err(e) => {
                            l.blocked.push(format!("{}: {e}", u.as_str()));
                            continue;
                        }
                    }
                }
                Ok(r) => {
                    l.blocked.push(format!("{}: HTTP {}", u.as_str(), r.status));
                    continue;
                }
                Err(e) => {
                    l.blocked.push(format!("{}: {e}", u.as_str()));
                    continue;
                }
            };
            let px = img.width as u64 * img.height as u64;
            if total_pixels + px > MAX_ALL_IMAGE_PIXELS {
                l.blocked.push("the page's pictures are too big; the rest were not loaded".into());
                break 'batches;
            }
            total_pixels += px;
            l.images.insert(*n, Arc::new(img));
        }
    }
    for (n, bytes, mime) in inline {
        if let Ok(img) = image::decode(&bytes, Some(&mime)) {
            let px = img.width as u64 * img.height as u64;
            if total_pixels + px > MAX_ALL_IMAGE_PIXELS {
                l.blocked.push("the page's pictures are too big; the rest were not loaded".into());
                break;
            }
            total_pixels += px;
            l.images.insert(n, Arc::new(img));
        }
    }
    Ok(())
}

/// The largest candidate in a `srcset`, as a URL.
fn first_srcset(s: Option<&str>) -> Option<String> {
    let mut best: Option<(u32, String)> = None;
    for part in s?.split(',') {
        let mut words = part.split_whitespace();
        let url = words.next()?;
        let desc = words.next().unwrap_or("1x");
        let score = if let Some(w) = desc.strip_suffix('w') {
            w.parse::<u32>().unwrap_or(0)
        } else if let Some(x) = desc.strip_suffix('x') {
            (x.parse::<f32>().unwrap_or(1.0) * 1000.0) as u32
        } else {
            0
        };
        if best.as_ref().is_none_or(|(b, _)| score > *b) {
            best = Some((score, url.to_string()));
        }
    }
    best.map(|(_, u)| u)
}

/// A `data:` URL: base64 or percent-encoded bytes, never more than one image.
fn data_url(s: &str) -> Option<(Vec<u8>, String)> {
    let rest = s.strip_prefix("data:")?;
    let (meta, payload) = rest.split_once(',')?;
    let mut mime = meta.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    if mime.is_empty() {
        mime = "application/octet-stream".into();
    }
    let base64 = meta.split(';').any(|p| p.eq_ignore_ascii_case("base64"));
    let bytes = if base64 { base64_decode(payload)? } else { percent_bytes(payload) };
    if bytes.len() > MAX_IMAGE_BYTES {
        return None;
    }
    Some((bytes, mime))
}

fn percent_bytes(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or(""), 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u8> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        })
    }
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut acc = 0u32;
    let mut n = 0u32;
    for &c in s.as_bytes() {
        if c.is_ascii_whitespace() || c == b'=' {
            continue;
        }
        let v = val(c)? as u32;
        acc = (acc << 6) | v;
        n += 6;
        if n >= 8 {
            n -= 8;
            out.push((acc >> n) as u8);
            acc &= (1 << n) - 1;
        }
    }
    Some(out)
}


fn gather_scripts(l: &mut Loaded, fetch: Fetcher, cancel: &AtomicBool) -> Result<(), LoadError> {
    let (Some(base), Location::Web(page)) = (l.base.clone(), l.location.clone()) else { return Ok(()) };
    let mut first: Vec<PageScript> = Vec::new();
    let mut later: Vec<PageScript> = Vec::new();
    // (index into `first` or `later`, is later, url) for external scripts.
    let mut wanted: Vec<(bool, usize, Url)> = Vec::new();
    let mut inline_bytes = 0usize;
    for n in l.doc.find_all("script") {
        if first.len() + later.len() >= MAX_SCRIPTS {
            l.blocked.push("too many scripts; the rest were not loaded".into());
            break;
        }
        let Some(e) = l.doc.element(n) else { continue };
        if e.foreign || !crate::script::dom::runnable_type(e) {
            continue;
        }
        match e.attr("src").map(|s| s.trim().to_string()) {
            Some(src) if !src.is_empty() => {
                let deferred = e.attr("async").is_some() || e.attr("defer").is_some();
                let list = if deferred { &mut later } else { &mut first };
                let source = match base.join(&src) {
                    Err(_) => Source::Refused(format!("{src}: bad address")),
                    Ok(u) => {
                        if u.scheme() != "https" && !(l.allow_http && u.scheme() == "http" && page.scheme() == "http") {
                            Source::Refused(format!("{}: scripts load only over https", u.as_str()))
                        } else if l.script_mode != Mode::All && !crate::script::dom::same_site(&page, &u) {
                            Source::Refused(format!("{}: another site's script (allowed with the scripts button)", u.as_str()))
                        } else {
                            wanted.push((deferred, list.len(), u));
                            Source::Refused(String::new())
                        }
                    }
                };
                list.push(PageScript { node: n, source });
            }
            Some(_) => {}
            None => {
                let text = l.doc.text_content(n);
                inline_bytes += text.len();
                if inline_bytes > MAX_ALL_SCRIPT_BYTES {
                    continue;
                }
                first.push(PageScript { node: n, source: Source::Inline(text) });
            }
        }
    }
    // Fetch external scripts, six at a time.
    let mut total = inline_bytes;
    for batch in wanted.chunks(6) {
        if cancel.load(Ordering::Relaxed) {
            return Err(LoadError::Cancelled);
        }
        let results: Vec<Result<Response, FetchError>> = std::thread::scope(|sc| {
            let handles: Vec<_> = batch
                .iter()
                .map(|(_, _, u)| {
                    let mut req = Request::get(u.clone());
                    req.max_bytes = MAX_SCRIPT_BYTES;
                    req.allow_http = l.allow_http && u.scheme() == "http";
                    std::thread::Builder::new().stack_size(4 << 20).spawn_scoped(sc, move || fetch(&req))
                })
                .collect();
            handles.into_iter().map(|h| match h {
                Ok(h) => h.join().unwrap_or(Err(FetchError::Timeout)),
                Err(_) => Err(FetchError::Timeout),
            }).collect()
        });
        for ((deferred, i, u), r) in batch.iter().zip(results) {
            let source = match r {
                Ok(r) if (200..300).contains(&r.status) => {
                    if !r.secure {
                        l.secure = false;
                    }
                    total += r.body.len();
                    if total > MAX_ALL_SCRIPT_BYTES {
                        Source::Refused(format!("{}: the page's scripts are too big", u.as_str()))
                    } else {
                        let text = html::decode_as(r.body.strip_prefix(b"\xEF\xBB\xBF".as_slice()).unwrap_or(&r.body), Charset::Utf8);
                        Source::Fetched { url: r.url.as_str().to_string(), text }
                    }
                }
                Ok(r) => Source::Refused(format!("{}: HTTP {}", u.as_str(), r.status)),
                Err(e) => Source::Refused(format!("{}: {e}", u.as_str())),
            };
            let list = if *deferred { &mut later } else { &mut first };
            list[*i].source = source;
        }
    }
    first.extend(later);
    l.scripts = first;
    Ok(())
}

fn media_ok(m: &str, env: &MediaEnv) -> bool {
    let cvs = crate::css::tokenizer::component_values(crate::css::tokenizer::tokenize(m));
    crate::css::parser::media_matches(&cvs, env)
}

/// Adds `sheet` after the sheets it imports (imports come first in the cascade).
fn push_sheet(l: &mut Loaded, sheet: Stylesheet, base: Option<&Url>, fetch: Option<Fetcher>, cancel: &AtomicBool, depth: usize) -> Result<(), LoadError> {
    if depth < MAX_IMPORT_DEPTH {
        if let (Some(base), Some(f)) = (base, fetch) {
            for imp in sheet.imports.iter().take(16) {
                if l.sheets.len() >= MAX_SHEETS {
                    break;
                }
                if let Some((s, url)) = fetch_sheet(l, base, imp, f, cancel)? {
                    push_sheet(l, s, Some(&url), fetch, cancel, depth + 1)?;
                }
            }
        }
    }
    if l.sheets.len() < MAX_SHEETS {
        l.sheets.push(sheet);
    }
    Ok(())
}

fn fetch_sheet(l: &mut Loaded, base: &Url, href: &str, fetch: Fetcher, cancel: &AtomicBool) -> Result<Option<(Stylesheet, Url)>, LoadError> {
    if cancel.load(Ordering::Relaxed) {
        return Err(LoadError::Cancelled);
    }
    let Ok(url) = base.join(href) else {
        l.blocked.push(format!("{href}: bad address"));
        return Ok(None);
    };
    let mut req = Request::get(url.clone());
    req.max_bytes = MAX_SHEET_BYTES;
    // An https page never loads http stylesheets (mixed content); a page the
    // user chose to open over http may.
    req.allow_http = l.allow_http && base.scheme() == "http";
    match fetch(&req) {
        Ok(r) if (200..300).contains(&r.status) => {
            if !r.secure {
                l.secure = false;
            }
            let text = html::decode_as(r.body.strip_prefix(b"\xEF\xBB\xBF".as_slice()).unwrap_or(&r.body), Charset::Utf8);
            Ok(Some((Stylesheet::parse(&text, &MediaEnv::default()), r.url)))
        }
        Ok(r) => {
            l.blocked.push(format!("{}: HTTP {}", url.as_str(), r.status));
            Ok(None)
        }
        Err(e) => {
            l.blocked.push(format!("{}: {e}", url.as_str()));
            Ok(None)
        }
    }
}

/// Lays a loaded page out for a viewport.
pub fn lay_out(l: &Loaded, width: f32, height: f32) -> Page {
    let env = MediaEnv { width, height };
    // Media queries in sheets were evaluated at parse time against the
    // default environment; re-parse is avoided by keeping the default close
    // to the window size (the browser's window is fixed-size on æther).
    let styles = compute_styles(&l.doc, &l.sheets, &env);
    let out = build_tree(&l.doc, &styles);
    let sizes: HashMap<NodeId, (u32, u32)> = l.images.iter().map(|(&n, i)| (n, (i.width, i.height))).collect();
    apply_image_sizes(&out.root, &sizes);
    layout_document(out, width, height)
}

/// Initial form state: the value attributes of text fields and textareas'
/// text, and checkbox states.
pub fn initial_form(doc: &Document) -> FormState {
    let mut f = FormState::default();
    for n in 0..doc.nodes.len() {
        let Some(e) = doc.element(n) else { continue };
        match e.name.as_str() {
            "input" => {
                let t = e.attr("type").unwrap_or("text").to_ascii_lowercase();
                if t == "checkbox" || t == "radio" {
                    f.checked.insert(n, e.attr("checked").is_some());
                } else if let Some(v) = e.attr("value") {
                    f.values.insert(n, v.chars().filter(|c| !c.is_control()).take(4096).collect());
                }
            }
            "textarea" => {
                f.values.insert(n, doc.text_content(n).chars().take(65536).collect());
            }
            _ => {}
        }
    }
    f
}

pub fn form_of(doc: &Document, n: NodeId) -> Option<NodeId> {
    let mut p = doc.parent(n);
    while let Some(x) = p {
        if doc.is(x, "form") {
            return Some(x);
        }
        p = doc.parent(x);
    }
    None
}

/// application/x-www-form-urlencoded.
pub fn urlencode(s: &str) -> String {
    let mut o = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'*' => o.push(b as char),
            b' ' => o.push('+'),
            _ => o.push_str(&format!("%{b:02X}")),
        }
    }
    o
}

/// The request a form submission makes (from the field or button `from`),
/// or None when there is no form or no usable action.
pub fn submit_form(l: &Loaded, form: &FormState, from: NodeId) -> Option<Request> {
    let doc = &l.doc;
    let f = form_of(doc, from)?;
    let fe = doc.element(f)?;
    let base = l.base.as_ref()?;
    let action = fe.attr("action").map(str::trim).filter(|a| !a.is_empty()).unwrap_or("");
    let target = if action.is_empty() { l.base.clone()? } else { base.join(action).ok()? };
    let mut pairs: Vec<(String, String)> = Vec::new();
    for n in doc.descendants(f) {
        let Some(e) = doc.element(n) else { continue };
        if e.attr("disabled").is_some() {
            continue;
        }
        let Some(name) = e.attr("name").filter(|s| !s.is_empty()) else { continue };
        match e.name.as_str() {
            "input" => {
                let t = e.attr("type").unwrap_or("text").to_ascii_lowercase();
                match t.as_str() {
                    "checkbox" | "radio" => {
                        if form.checked.get(&n).copied().unwrap_or(false) {
                            pairs.push((name.into(), e.attr("value").unwrap_or("on").into()));
                        }
                    }
                    "submit" | "image" => {
                        if n == from {
                            pairs.push((name.into(), e.attr("value").unwrap_or("").into()));
                        }
                    }
                    "button" | "reset" | "file" => {}
                    "hidden" => pairs.push((name.into(), e.attr("value").unwrap_or("").into())),
                    _ => pairs.push((name.into(), form.values.get(&n).cloned().unwrap_or_default())),
                }
            }
            "textarea" => pairs.push((name.into(), form.values.get(&n).cloned().unwrap_or_default())),
            "button" => {
                if n == from {
                    pairs.push((name.into(), e.attr("value").unwrap_or("").into()));
                }
            }
            "select" => {
                let opts: Vec<NodeId> = doc.descendants(n).into_iter().filter(|&o| doc.is(o, "option")).collect();
                let sel = opts.iter().copied().find(|&o| doc.element(o).is_some_and(|e| e.attr("selected").is_some())).or(opts.first().copied());
                if let Some(o) = sel {
                    let v = doc.element(o).and_then(|e| e.attr("value")).map(|s| s.to_string()).unwrap_or_else(|| doc.text_content(o).trim().to_string());
                    pairs.push((name.into(), v));
                }
            }
            _ => {}
        }
        if pairs.len() > 256 {
            break;
        }
    }
    let query: Vec<String> = pairs.iter().map(|(k, v)| format!("{}={}", urlencode(k), urlencode(v))).collect();
    let query = query.join("&");
    let post = fe.attr("method").is_some_and(|m| m.trim().eq_ignore_ascii_case("post"));
    let mut req = if post {
        let mut r = Request::get(target);
        r.method = Method::Post;
        r.body = query.into_bytes();
        r
    } else {
        let s = target.as_str();
        let s = s.split('#').next().unwrap_or(s);
        let s = s.split('?').next().unwrap_or(s);
        Request::get(Url::parse(&format!("{s}?{query}")).ok()?)
    };
    req.allow_http = l.allow_http;
    Some(req)
}

/// The node a fragment names: an element with that id, or an <a name>.
pub fn fragment_target(doc: &Document, frag: &str) -> Option<NodeId> {
    if frag.is_empty() || frag.eq_ignore_ascii_case("top") {
        return None;
    }
    let decoded = percent_decode(frag);
    (0..doc.nodes.len())
        .find(|&n| doc.element(n).is_some_and(|e| e.id() == Some(decoded.as_str())))
        .or_else(|| (0..doc.nodes.len()).find(|&n| doc.is(n, "a") && doc.element(n).is_some_and(|e| e.attr("name") == Some(decoded.as_str()))))
}

pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let h = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok());
            if let Some(h) = h {
                out.push(h);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake(r: &Request) -> Result<Response, FetchError> {
        let path = r.url.as_str().to_string();
        let (ct, body): (&str, &[u8]) = if path.ends_with("/a.css") {
            ("text/css", b"@import 'b.css'; h1 { color: red }")
        } else if path.ends_with("/b.css") {
            ("text/css", b"@import 'b.css'; p { color: blue }")
        } else if path.ends_with("/page") {
            ("text/html", b"<title>T</title><link rel=stylesheet href=a.css><link rel='alternate stylesheet' href=c.css><style media=print>p{}</style><style>em{}</style><h1>x</h1>")
        } else if path.ends_with("/latin") {
            ("text/html; charset=iso-8859-1", b"<title>caf\xe9</title>")
        } else if path.ends_with("/img") {
            ("image/png", b"\x89PNG")
        } else {
            return Err(FetchError::Timeout);
        };
        if r.url.scheme() == "http" && !r.allow_http {
            return Err(FetchError::InsecureHttp(r.url.clone()));
        }
        Ok(Response { url: r.url.clone(), status: 200, headers: vec![("Content-Type".into(), ct.into())], body: body.to_vec(), secure: r.url.scheme() == "https" })
    }

    fn get(u: &str) -> Result<Loaded, LoadError> {
        load(Request::get(Url::parse(u).unwrap()), None, &fake, &AtomicBool::new(false), &Policy::default())
    }

    #[test]
    fn stylesheets_and_imports() {
        let l = get("https://x.test/page").unwrap();
        assert_eq!(l.title, "T");
        // b.css imports itself: bounded by the depth limit. Then a.css, then <style>.
        assert!(l.sheets.len() >= 3 && l.sheets.len() <= MAX_IMPORT_DEPTH + 3, "{}", l.sheets.len());
        assert!(l.secure);
        let page = lay_out(&l, 800.0, 600.0);
        assert!(page.text().contains('x'));
    }

    #[test]
    fn charset_and_types() {
        assert_eq!(get("https://x.test/latin").unwrap().title, "café");
        assert!(matches!(get("https://x.test/img"), Err(LoadError::Unsupported(_))));
        assert!(matches!(get("http://x.test/page"), Err(LoadError::Fetch(FetchError::InsecureHttp(_)))));
    }

    #[test]
    fn forms_submit() {
        let html = "<form action=/search><input name=q value='a b'><input type=hidden name=h value=1><input type=checkbox name=c checked><input type=submit name=go value=Go></form>";
        let mut l = load_builtin("t", html);
        l.base = Some(Url::parse("https://x.test/dir/page").unwrap());
        let f = initial_form(&l.doc);
        let submit = (0..l.doc.nodes.len()).find(|&n| l.doc.element(n).is_some_and(|e| e.attr("type") == Some("submit"))).unwrap();
        let r = submit_form(&l, &f, submit).unwrap();
        assert_eq!(r.url.as_str(), "https://x.test/search?q=a+b&h=1&c=on&go=Go");
        assert_eq!(urlencode("é&="), "%C3%A9%26%3D");
    }

    #[test]
    fn fragments() {
        let l = load_builtin("t", "<p id=a>x<a name=b%20c>y</a>");
        assert!(fragment_target(&l.doc, "a").is_some());
        assert!(fragment_target(&l.doc, "b%2520c").is_some());
        assert!(fragment_target(&l.doc, "zz").is_none());
        assert_eq!(percent_decode("%4"), "%4");
        assert_eq!(percent_decode("%"), "%");
    }
}
