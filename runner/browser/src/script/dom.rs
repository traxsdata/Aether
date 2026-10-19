//! script/dom.rs - what a page's scripts see of the page: `document`, its
//! nodes and elements, events, and the host state the browser reads back
//! (did the page change, does it want to navigate, scroll, submit...).
//!
//! Scripts never touch the browser directly. They change the document and
//! leave requests in `Host`; the window applies them after each task. The
//! document itself lives in the page and is lent to the host for the
//! duration of a task (see script::Runtime).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::mpsc::Receiver;
use std::time::Instant;

use super::interp::{Args, Ctrl, ErrKind, Interp, R};
use super::value::*;
use crate::css::parser::Selector;
use crate::dom::{Document, Element, NodeData, NodeId};
use crate::net::{FetchError, Request, Response, Url};
use crate::page::Fetcher;
use crate::paint::FormState;

type A<'a> = Args<'a>;

/// Nodes one document may hold (scripts can create more; not without end).
pub const MAX_NODES: usize = 1_000_000;
pub const MAX_TIMERS: usize = 10_000;
/// Requests one page may make from scripts (fetch, XMLHttpRequest, scripts).
pub const MAX_FETCHES: usize = 2_000;
pub const MAX_ACTIVE_FETCHES: usize = 6;
pub const MAX_SCRIPT_BYTES: usize = 4 << 20;
const MAX_LOG: usize = 500;

/// Storage that lives as long as the browser runs, per origin. Nothing in
/// it is ever written to disk or sent anywhere (cookies set by scripts stay
/// in the page: the browser sends no cookies at all).
#[derive(Default, Debug)]
pub struct Storage {
    pub local: HashMap<String, Vec<(String, String)>>,
    pub session: HashMap<String, Vec<(String, String)>>,
    pub cookies: HashMap<String, Vec<(String, String)>>,
}

pub const STORAGE_QUOTA: usize = 5 << 20;

pub struct Timer {
    pub id: u32,
    pub due: f64,
    pub every: Option<f64>,
    pub f: Value,
    pub args: Vec<Value>,
}

/// Who is waiting for a request.
#[derive(Clone, Debug)]
pub enum FetchFor {
    Promise(Gc),
    Xhr(Gc),
    Script(NodeId),
}

pub struct Pending {
    /// Waiting for a free slot.
    pub queued: Option<Request>,
    pub rx: Option<Receiver<Result<Response, FetchError>>>,
    pub for_: FetchFor,
}

/// Navigation a script asked for.
#[derive(Debug, Clone, PartialEq)]
pub enum NavReq {
    Go { url: String, replace: bool },
    Reload,
    Back,
    Forward,
}

pub struct Host {
    pub doc: Document,
    pub form: FormState,
    pub url: Option<Url>,
    pub base: Option<Url>,
    /// Scripts from other sites may load.
    pub third_party: bool,
    pub fetcher: Option<Fetcher<'static>>,
    pub storage: Rc<RefCell<Storage>>,
    /// One wrapper object per node, so `a === b` works and expandos stick.
    pub wrappers: HashMap<NodeId, Gc>,
    /// The document changed: lay it out again.
    pub dirty: bool,
    /// Something visible changed that doesn't need layout (field values).
    pub repaint: bool,
    pub nav: Option<NavReq>,
    /// history.pushState/replaceState moved the address without loading.
    pub url_changed: Option<(Url, bool)>,
    pub submit: Option<NodeId>,
    pub focus_req: Option<Option<NodeId>>,
    pub focused: Option<NodeId>,
    pub scroll: (f32, f32),
    pub scroll_req: Option<(f32, f32)>,
    pub viewport: (f32, f32),
    pub page_h: f32,
    /// Border boxes from the last layout, in page coordinates.
    pub rects: HashMap<NodeId, [f32; 4]>,
    pub console: Vec<String>,
    pub errors: Vec<String>,
    pub alerts: Vec<String>,
    pub timers: Vec<Timer>,
    pub next_id: u32,
    pub fetches: Vec<Pending>,
    pub fetch_count: usize,
    /// Script elements that have run (or been refused); they never run twice.
    pub started: HashSet<NodeId>,
    pub current_script: Option<NodeId>,
    /// While the page's own scripts run: where document.write puts markup.
    pub write_point: Option<NodeId>,
    pub ready: &'static str,
    selectors: HashMap<String, Option<Rc<Vec<Selector>>>>,
    start: Instant,
    /// Time skipped ahead (an await on a timer, or a headless test).
    pub warp: f64,
}

impl Default for Host {
    fn default() -> Self {
        Host {
            doc: Document::default(),
            form: FormState::default(),
            url: None,
            base: None,
            third_party: false,
            fetcher: None,
            storage: Rc::default(),
            wrappers: HashMap::new(),
            dirty: false,
            repaint: false,
            nav: None,
            url_changed: None,
            submit: None,
            focus_req: None,
            focused: None,
            scroll: (0.0, 0.0),
            scroll_req: None,
            viewport: (1024.0, 768.0),
            page_h: 0.0,
            rects: HashMap::new(),
            console: Vec::new(),
            errors: Vec::new(),
            alerts: Vec::new(),
            timers: Vec::new(),
            next_id: 1,
            fetches: Vec::new(),
            fetch_count: 0,
            started: HashSet::new(),
            current_script: None,
            write_point: None,
            ready: "loading",
            selectors: HashMap::new(),
            start: Instant::now(),
            warp: 0.0,
        }
    }
}

impl Host {
    pub fn roots(&self) -> Vec<Value> {
        let mut v: Vec<Value> = self.wrappers.values().map(|g| Value::Obj(*g)).collect();
        for t in &self.timers {
            v.push(t.f.clone());
            v.extend(t.args.iter().cloned());
        }
        for p in &self.fetches {
            match p.for_ {
                FetchFor::Promise(g) | FetchFor::Xhr(g) => v.push(Value::Obj(g)),
                FetchFor::Script(_) => {}
            }
        }
        v
    }

    /// Milliseconds since the page's scripts started.
    pub fn now(&self) -> f64 {
        self.start.elapsed().as_secs_f64() * 1000.0 + self.warp
    }

    /// "scheme://host[:port]", or "null" for built-in pages.
    pub fn origin(&self) -> String {
        match &self.url {
            Some(u) => origin_of(u),
            None => "null".into(),
        }
    }

    pub fn log(&mut self, s: String) {
        if std::env::var_os("BROWSER_CONSOLE").is_some() {
            eprintln!("console: {s}");
        }
        if self.console.len() >= MAX_LOG {
            self.console.remove(0);
        }
        self.console.push(s.chars().take(2000).collect());
    }

    pub fn error(&mut self, s: String) {
        self.log(s.clone());
        if self.errors.len() < MAX_LOG {
            self.errors.push(s);
        }
    }

    pub fn resolve(&self, href: &str) -> Option<Url> {
        match &self.base {
            Some(b) => b.join(href.trim()).ok(),
            None => Url::parse(href.trim()).ok(),
        }
    }
}

pub fn origin_of(u: &Url) -> String {
    match u.port() {
        Some(p) => format!("{}://{}:{p}", u.scheme(), u.host()),
        None => format!("{}://{}", u.scheme(), u.host()),
    }
}

/// The registrable part of a host name, roughly: the last two labels, or
/// three under a short second-level label (example.co.uk). IP addresses
/// stand for themselves.
pub fn site_of(host: &str) -> String {
    let h = host.trim_end_matches('.').to_ascii_lowercase();
    if h.parse::<std::net::Ipv4Addr>().is_ok() || h.contains(':') {
        return h;
    }
    let labels: Vec<&str> = h.split('.').collect();
    let n = labels.len();
    if n <= 2 {
        return h;
    }
    let sld = labels[n - 2];
    let take = if labels[n - 1].len() == 2 && (sld.len() <= 3 || matches!(sld, "gov" | "com" | "net" | "org" | "edu" | "ac" | "co")) { 3 } else { 2 };
    labels[n.saturating_sub(take)..].join(".")
}

pub fn same_site(a: &Url, b: &Url) -> bool {
    a.scheme() == b.scheme() && site_of(a.host()) == site_of(b.host())
}

// ---- wrappers -------------------------------------------------------------------

pub fn wrap(it: &mut Interp, n: NodeId) -> Value {
    if let Some(g) = it.host.wrappers.get(&n) {
        return Value::Obj(*g);
    }
    let p = it.p.node;
    let g = it.alloc(Obj::new(p, Kind::Node(n)));
    it.host.wrappers.insert(n, g);
    Value::Obj(g)
}

fn wrap_opt(it: &mut Interp, n: Option<NodeId>) -> Value {
    match n {
        Some(n) => wrap(it, n),
        None => Value::Null,
    }
}

pub fn node_of(it: &Interp, v: &Value) -> Option<NodeId> {
    match v {
        Value::Obj(g) => match it.heap.get(*g).kind {
            Kind::Node(n) => Some(n),
            _ => None,
        },
        _ => None,
    }
}

fn this_node(it: &mut Interp, a: &A) -> R<NodeId> {
    match node_of(it, &a.this) {
        Some(n) => Ok(n),
        None => it.type_err("Illegal invocation: not a node"),
    }
}

fn arg_node(it: &mut Interp, v: &Value) -> R<NodeId> {
    match node_of(it, v) {
        Some(n) => Ok(n),
        None => it.type_err("parameter is not of type 'Node'"),
    }
}

fn s(v: &str) -> Value {
    Value::str(v)
}

fn str_arg(it: &mut Interp, a: &A, i: usize) -> R<String> {
    Ok(it.to_str(&a.get(i))?.to_string())
}

fn el<'a>(it: &'a Interp, n: NodeId) -> Option<&'a Element> {
    it.host.doc.element(n)
}

fn attr(it: &Interp, n: NodeId, name: &str) -> Option<String> {
    el(it, n).and_then(|e| e.attr(name)).map(|s| s.to_string())
}

pub fn set_attr(it: &mut Interp, n: NodeId, name: &str, v: &str) {
    if let Some(e) = it.host.doc.element_mut(n) {
        let v: String = v.chars().take(1 << 20).collect();
        match e.attrs.iter_mut().find(|(k, _)| k == name) {
            Some(slot) => slot.1 = v,
            None => e.attrs.push((name.to_string(), v)),
        }
        it.host.dirty = true;
    }
}

fn remove_attr(it: &mut Interp, n: NodeId, name: &str) {
    if let Some(e) = it.host.doc.element_mut(n) {
        let before = e.attrs.len();
        e.attrs.retain(|(k, _)| k != name);
        if e.attrs.len() != before {
            it.host.dirty = true;
        }
    }
}

fn is_el(it: &Interp, n: NodeId, name: &str) -> bool {
    it.host.doc.is(n, name)
}

fn connected(doc: &Document, n: NodeId) -> bool {
    let mut cur = n;
    let mut hops = 0;
    loop {
        if cur == Document::ROOT {
            return true;
        }
        match doc.parent(cur) {
            Some(p) => cur = p,
            None => return false,
        }
        hops += 1;
        if hops > 100_000 {
            return false;
        }
    }
}

fn new_node(it: &mut Interp, data: NodeData) -> R<NodeId> {
    if it.host.doc.nodes.len() >= MAX_NODES {
        return Err(it.err(ErrKind::Range, "the page has too many nodes"));
    }
    Ok(it.host.doc.new_node(data))
}

fn new_element(it: &mut Interp, name: &str) -> R<NodeId> {
    let name = name.trim().to_ascii_lowercase();
    if name.is_empty() || name.chars().any(|c| c.is_whitespace() || c == '<' || c == '>') {
        return Err(it.err(ErrKind::Error, "InvalidCharacterError: the tag name is not valid"));
    }
    new_node(it, NodeData::Element(Element { name, attrs: Vec::new(), foreign: false }))
}

const FRAGMENT: &str = "#document-fragment";

fn is_fragment(it: &Interp, n: NodeId) -> bool {
    el(it, n).is_some_and(|e| e.name == FRAGMENT)
}

// ---- tree changes -----------------------------------------------------------------

/// Is `a` an inclusive ancestor of `b`?
fn contains(doc: &Document, a: NodeId, b: NodeId) -> bool {
    let mut cur = Some(b);
    let mut hops = 0;
    while let Some(c) = cur {
        if c == a {
            return true;
        }
        cur = doc.parent(c);
        hops += 1;
        if hops > 100_000 {
            break;
        }
    }
    false
}

/// Inserts `child` (a fragment's children, if it is one) into `parent`
/// before `before` (None: at the end). Runs scripts that become connected.
fn insert(it: &mut Interp, parent: NodeId, child: NodeId, before: Option<NodeId>) -> R<()> {
    if matches!(it.host.doc.nodes[parent].data, NodeData::Text(_) | NodeData::Comment(_) | NodeData::Doctype(_)) {
        return Err(it.err(ErrKind::Error, "HierarchyRequestError: this node can't have children"));
    }
    if contains(&it.host.doc, child, parent) {
        return Err(it.err(ErrKind::Error, "HierarchyRequestError: the new child contains the parent"));
    }
    if let Some(b) = before {
        if it.host.doc.parent(b) != Some(parent) {
            return Err(it.err(ErrKind::Error, "NotFoundError: the reference node is not a child of this node"));
        }
    }
    let kids: Vec<NodeId> = if is_fragment(it, child) { it.host.doc.children(child).to_vec() } else { vec![child] };
    for k in &kids {
        match before {
            Some(b) if b != *k => it.host.doc.insert_before(parent, *k, b),
            Some(_) => {}
            None => it.host.doc.append(parent, *k),
        }
    }
    it.host.dirty = true;
    if connected(&it.host.doc, parent) {
        for k in kids {
            start_scripts_in(it, k)?;
        }
    }
    Ok(())
}

fn remove_node(it: &mut Interp, n: NodeId) {
    if it.host.doc.parent(n).is_some() {
        it.host.doc.detach(n);
        it.host.dirty = true;
    }
}

fn remove_children(it: &mut Interp, n: NodeId) {
    let kids = it.host.doc.children(n).to_vec();
    for k in kids {
        it.host.doc.detach(k);
    }
    it.host.dirty = true;
}

/// Script elements under `n` that just became connected start now.
fn start_scripts_in(it: &mut Interp, n: NodeId) -> R<()> {
    let scripts: Vec<NodeId> = it.host.doc.descendants(n).into_iter().filter(|&d| it.host.doc.is(d, "script")).collect();
    for sc in scripts {
        if it.host.started.contains(&sc) {
            continue;
        }
        start_script(it, sc)?;
    }
    Ok(())
}

/// Is this a classic script we run? (No modules, no JSON blocks, no templates.)
pub fn runnable_type(e: &Element) -> bool {
    let t = e.attr("type").map(|t| t.trim().to_ascii_lowercase()).unwrap_or_default();
    let lang = e.attr("language").map(|t| t.trim().to_ascii_lowercase()).unwrap_or_default();
    let type_ok = matches!(
        t.as_str(),
        "" | "text/javascript" | "application/javascript" | "text/ecmascript" | "application/ecmascript" | "application/x-javascript" | "text/x-javascript" | "text/jscript"
    );
    type_ok && (lang.is_empty() || lang.starts_with("javascript") || t != "")
}

fn start_script(it: &mut Interp, sc: NodeId) -> R<()> {
    it.host.started.insert(sc);
    let Some(e) = el(it, sc) else { return Ok(()) };
    if !runnable_type(e) {
        return Ok(());
    }
    if let Some(src) = e.attr("src").map(|s| s.to_string()) {
        if src.trim().is_empty() {
            return Ok(());
        }
        match it.host.resolve(&src) {
            Some(u) => {
                if let Err(why) = script_allowed(&it.host, &u) {
                    it.host.log(format!("script {} not loaded: {why}", u.as_str()));
                    fire_simple(it, sc, "error")?;
                    return Ok(());
                }
                let mut req = Request::get(u);
                req.max_bytes = MAX_SCRIPT_BYTES;
                start_fetch(it, req, FetchFor::Script(sc));
            }
            None => {
                fire_simple(it, sc, "error")?;
            }
        }
        return Ok(());
    }
    let text = it.host.doc.text_content(sc);
    run_source(it, &text, Some(sc))
}

/// Whether scripts from `u` may run on this page.
pub fn script_allowed(h: &Host, u: &Url) -> Result<(), &'static str> {
    let Some(page) = &h.url else { return Err("this page doesn't run scripts") };
    if u.scheme() != "https" && !(u.scheme() == page.scheme() && u.host() == page.host()) {
        return Err("scripts load only over https");
    }
    if !h.third_party && !same_site(page, u) {
        return Err("it is from another site (allow other sites' scripts with the scripts button)");
    }
    Ok(())
}

/// Parses and runs `src` as a script (inline, fetched, or written).
pub fn run_source(it: &mut Interp, src: &str, el: Option<NodeId>) -> R<()> {
    let prog = match super::parser::parse_program(src) {
        Ok(p) => p,
        Err(e) => {
            it.host.error(format!("{e}"));
            if let Some(n) = el {
                fire_simple(it, n, "error")?;
            }
            return Ok(());
        }
    };
    let saved_env = it.env;
    let saved_script = std::mem::replace(&mut it.host.current_script, el);
    let r = it.run_program(&prog);
    it.env = saved_env;
    it.host.current_script = saved_script;
    match r {
        Ok(()) => Ok(()),
        Err(Ctrl::Abort(m)) => Err(Ctrl::Abort(m)),
        Err(e) => {
            report(it, e);
            Ok(())
        }
    }
}

/// Logs an uncaught exception.
pub fn report(it: &mut Interp, e: Ctrl) {
    let msg = match e {
        Ctrl::Throw(v) => describe_error(it, &v),
        Ctrl::Abort(m) => m,
        other => format!("{other:?}"),
    };
    it.host.error(format!("Uncaught {msg}"));
}

pub fn describe_error(it: &mut Interp, v: &Value) -> String {
    if let Value::Obj(g) = v {
        if matches!(it.heap.get(*g).kind, Kind::Error) {
            let name = it.get(v, "name").ok().and_then(|n| it.to_str(&n).ok()).unwrap_or_else(|| "Error".into());
            let msg = it.get(v, "message").ok().and_then(|n| it.to_str(&n).ok()).unwrap_or_default();
            return if msg.is_empty() { name.to_string() } else { format!("{name}: {msg}") };
        }
    }
    let saved = it.step_limit;
    it.step_limit = it.steps + 10_000;
    let s = it.to_str(v).map(|s| s.to_string()).unwrap_or_else(|_| "an exception".into());
    it.step_limit = saved;
    s
}

// ---- fetching -----------------------------------------------------------------------

/// Starts (or queues) a request on its own thread.
pub fn start_fetch(it: &mut Interp, req: Request, for_: FetchFor) {
    it.host.fetch_count += 1;
    if it.host.fetch_count > MAX_FETCHES {
        it.host.fetches.push(Pending { queued: None, rx: None, for_ });
        return;
    }
    it.host.fetches.push(Pending { queued: Some(req), rx: None, for_ });
    launch_queued(&mut it.host);
}

/// Starts queued requests while there are free slots.
pub fn launch_queued(h: &mut Host) {
    let active = h.fetches.iter().filter(|p| p.rx.is_some()).count();
    let mut free = MAX_ACTIVE_FETCHES.saturating_sub(active);
    for p in h.fetches.iter_mut() {
        if free == 0 {
            break;
        }
        if let Some(req) = p.queued.take() {
            let (tx, rx) = std::sync::mpsc::channel();
            match h.fetcher {
                Some(f) => {
                    let spawned = std::thread::Builder::new().name("script-fetch".into()).stack_size(4 << 20).spawn(move || {
                        let _ = tx.send(f(&req));
                    });
                    if spawned.is_err() {
                        continue;
                    }
                }
                None => {
                    let _ = tx.send(Err(FetchError::Timeout));
                }
            }
            p.rx = Some(rx);
            free -= 1;
        }
    }
}

/// Requests that finished (or can never start), removed from the list.
pub fn finished_fetches(h: &mut Host, wait_ms: u64) -> Vec<(FetchFor, Result<Response, FetchError>)> {
    let deadline = Instant::now() + std::time::Duration::from_millis(wait_ms);
    loop {
        let mut done = Vec::new();
        let mut i = 0;
        while i < h.fetches.len() {
            let p = &h.fetches[i];
            let r = match (&p.rx, &p.queued) {
                (Some(rx), _) => match rx.try_recv() {
                    Ok(r) => Some(r),
                    Err(std::sync::mpsc::TryRecvError::Empty) => None,
                    Err(_) => Some(Err(FetchError::Timeout)),
                },
                (None, None) => Some(Err(FetchError::TooLarge(0))),
                (None, Some(_)) => None,
            };
            match r {
                Some(r) => {
                    let p = h.fetches.remove(i);
                    done.push((p.for_, r));
                }
                None => i += 1,
            }
        }
        launch_queued(h);
        if !done.is_empty() || h.fetches.is_empty() || Instant::now() >= deadline {
            return done;
        }
        std::thread::sleep(std::time::Duration::from_millis(4));
    }
}

/// Hands a finished request to whoever waits for it.
pub fn deliver(it: &mut Interp, for_: FetchFor, r: Result<Response, FetchError>) -> R<()> {
    match for_ {
        FetchFor::Promise(p) => super::window::fetch_done(it, p, r),
        FetchFor::Xhr(x) => super::window::xhr_done(it, x, r),
        FetchFor::Script(sc) => {
            match r {
                Ok(resp) if (200..300).contains(&resp.status) => {
                    let text = crate::html::decode_as(resp.body.strip_prefix(b"\xEF\xBB\xBF".as_slice()).unwrap_or(&resp.body), crate::html::Charset::Utf8);
                    run_source(it, &text, Some(sc))?;
                    fire_simple(it, sc, "load")?;
                }
                Ok(resp) => {
                    it.host.log(format!("script {}: HTTP {}", resp.url.as_str(), resp.status));
                    fire_simple(it, sc, "error")?;
                }
                Err(e) => {
                    it.host.log(format!("script not loaded: {e}"));
                    fire_simple(it, sc, "error")?;
                }
            }
            Ok(())
        }
    }
}

/// Lets an `await` that is waiting on the outside world move: finishes a
/// request (waiting for one) or runs the next timer early. False when
/// nothing is pending, so the awaited promise can never settle.
pub fn progress(it: &mut Interp) -> R<bool> {
    if !it.host.fetches.is_empty() {
        let done = finished_fetches(&mut it.host, 20_000);
        if !done.is_empty() {
            for (f, r) in done {
                deliver(it, f, r)?;
            }
            return Ok(true);
        }
    }
    let Some(i) = (0..it.host.timers.len()).min_by(|&a, &b| it.host.timers[a].due.total_cmp(&it.host.timers[b].due)) else { return Ok(false) };
    let now = it.host.now();
    let due = it.host.timers[i].due;
    if due > now {
        it.host.warp += due - now;
    }
    fire_timer(it, i)?;
    Ok(true)
}

/// Runs timer `i` (re-arming an interval).
pub fn fire_timer(it: &mut Interp, i: usize) -> R<()> {
    let now = it.host.now();
    let t = &mut it.host.timers[i];
    let (f, args) = (t.f.clone(), t.args.clone());
    match t.every {
        Some(ms) => t.due = now + ms,
        None => {
            it.host.timers.remove(i);
        }
    }
    match it.call(&f, Value::Undef, args) {
        Ok(_) => Ok(()),
        Err(Ctrl::Abort(m)) => Err(Ctrl::Abort(m)),
        Err(e) => {
            report(it, e);
            Ok(())
        }
    }
}

// ---- events ---------------------------------------------------------------------------

/// What the browser fills in on events it fires.
#[derive(Default, Clone, Debug)]
pub struct EventInit {
    pub bubbles: bool,
    pub cancelable: bool,
    pub key: Option<String>,
    pub key_code: u32,
    pub client: Option<(f32, f32)>,
    pub button: i32,
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

pub fn new_event(it: &mut Interp, ty: &str, init: &EventInit) -> Gc {
    let p = it.p.event;
    let g = it.alloc(Obj::new(p, Kind::Event(EventState { prevented: false, stopped: false, stopped_now: false, cancelable: init.cancelable })));
    it.set_prop(g, "type", s(ty));
    it.set_prop(g, "bubbles", Value::Bool(init.bubbles));
    it.set_prop(g, "cancelable", Value::Bool(init.cancelable));
    it.set_prop(g, "isTrusted", Value::Bool(true));
    it.set_prop(g, "target", Value::Null);
    it.set_prop(g, "currentTarget", Value::Null);
    it.set_prop(g, "eventPhase", Value::Num(0.0));
    it.set_prop(g, "composed", Value::Bool(false));
    let now = it.host.now();
    it.set_prop(g, "timeStamp", Value::Num(now.floor()));
    it.set_prop(g, "detail", Value::Num(0.0));
    if let Some(k) = &init.key {
        it.set_prop(g, "key", s(k));
        let code = key_code_name(k);
        it.set_prop(g, "code", s(&code));
        it.set_prop(g, "keyCode", Value::Num(init.key_code as f64));
        it.set_prop(g, "which", Value::Num(init.key_code as f64));
        it.set_prop(g, "charCode", Value::Num(if k.chars().count() == 1 { init.key_code as f64 } else { 0.0 }));
        it.set_prop(g, "repeat", Value::Bool(false));
        it.set_prop(g, "isComposing", Value::Bool(false));
        it.set_prop(g, "location", Value::Num(0.0));
    }
    if let Some((x, y)) = init.client {
        for (k, v) in [("clientX", x), ("clientY", y), ("x", x), ("y", y), ("pageX", x + it.host.scroll.0), ("pageY", y + it.host.scroll.1), ("screenX", x), ("screenY", y), ("offsetX", 0.0), ("offsetY", 0.0), ("movementX", 0.0), ("movementY", 0.0)] {
            it.set_prop(g, k, Value::Num(v as f64));
        }
        it.set_prop(g, "button", Value::Num(init.button as f64));
        it.set_prop(g, "buttons", Value::Num(if init.button == 0 { 1.0 } else { 0.0 }));
        it.set_prop(g, "relatedTarget", Value::Null);
        it.set_prop(g, "detail", Value::Num(1.0));
    }
    if init.key.is_some() || init.client.is_some() {
        it.set_prop(g, "shiftKey", Value::Bool(init.shift));
        it.set_prop(g, "ctrlKey", Value::Bool(init.ctrl));
        it.set_prop(g, "altKey", Value::Bool(init.alt));
        it.set_prop(g, "metaKey", Value::Bool(false));
    }
    g
}

fn key_code_name(k: &str) -> String {
    let mut ch = k.chars();
    match (ch.next(), ch.next()) {
        (Some(c), None) if c.is_ascii_alphabetic() => format!("Key{}", c.to_ascii_uppercase()),
        (Some(c), None) if c.is_ascii_digit() => format!("Digit{c}"),
        (Some(' '), None) => "Space".into(),
        _ => k.to_string(),
    }
}

fn event_state(it: &Interp, ev: Gc) -> Option<EventState> {
    match &it.heap.get(ev).kind {
        Kind::Event(st) => Some(st.clone()),
        _ => None,
    }
}

fn with_state(it: &mut Interp, ev: Gc, f: impl FnOnce(&mut EventState)) {
    if let Kind::Event(st) = &mut it.heap.get_mut(ev).kind {
        f(st);
    }
}

/// The objects an event visits, target first: the node, its ancestors, the
/// document, then the window (only for nodes in the document).
fn event_path(it: &mut Interp, target: &Value) -> Vec<Value> {
    let Some(n) = node_of(it, target) else { return vec![target.clone()] };
    let mut chain = vec![n];
    let mut cur = n;
    while let Some(p) = it.host.doc.parent(cur) {
        chain.push(p);
        cur = p;
        if chain.len() > 10_000 {
            break;
        }
    }
    let mut path: Vec<Value> = chain.iter().map(|&c| wrap(it, c)).collect();
    if cur == Document::ROOT {
        path.push(Value::Obj(it.global));
    }
    path
}

/// Dispatches event `ev` at `target`; false if a listener cancelled it.
pub fn dispatch(it: &mut Interp, target: &Value, ev: Gc) -> R<bool> {
    let evv = Value::Obj(ev);
    let path = event_path(it, target);
    it.set_prop(ev, "target", target.clone());
    let bubbles = it.get(&evv, "bubbles").map(|v| Interp::truthy(&v)).unwrap_or(false);
    let ty = it.get(&evv, "type")?;
    let ty = it.to_str(&ty)?.to_string();
    let path_arr = it.new_array(path.clone());
    it.hide(ev, "@@path", path_arr);
    // Capture, from the window down.
    for i in (1..path.len()).rev() {
        invoke(it, &path[i], &evv, &ty, Some(true), 1.0)?;
    }
    invoke(it, &path[0], &evv, &ty, None, 2.0)?;
    if bubbles {
        for p in path.iter().skip(1) {
            invoke(it, p, &evv, &ty, Some(false), 3.0)?;
        }
    }
    it.set_prop(ev, "eventPhase", Value::Num(0.0));
    it.set_prop(ev, "currentTarget", Value::Null);
    Ok(!event_state(it, ev).is_some_and(|s| s.prevented))
}

/// Calls `cur`'s listeners for one phase (`capture` None: the target, both kinds).
fn invoke(it: &mut Interp, cur: &Value, ev: &Value, ty: &str, capture: Option<bool>, phase: f64) -> R<()> {
    let Value::Obj(evg) = ev else { return Ok(()) };
    if event_state(it, *evg).is_some_and(|s| s.stopped) {
        return Ok(());
    }
    it.set_prop(*evg, "currentTarget", cur.clone());
    it.set_prop(*evg, "eventPhase", Value::Num(phase));
    // The on<type> handler (a property, or an attribute compiled once).
    if capture != Some(true) {
        if let Some(h) = handler(it, cur, ty)? {
            let r = it.call(&h, cur.clone(), vec![ev.clone()]);
            match r {
                Ok(Value::Bool(false)) => with_state(it, *evg, |s| {
                    if s.cancelable {
                        s.prevented = true
                    }
                }),
                Ok(_) => {}
                Err(Ctrl::Abort(m)) => return Err(Ctrl::Abort(m)),
                Err(e) => report(it, e),
            }
        }
    }
    let Value::Obj(cg) = cur else { return Ok(()) };
    let list = match it.heap.get(*cg).props.get("@@listeners") {
        Some(Slot::Data(Value::Obj(l), _)) => *l,
        _ => return Ok(()),
    };
    let entries: Vec<Value> = match &it.heap.get(list).kind {
        Kind::Array(v) => v.clone(),
        _ => return Ok(()),
    };
    for e in entries {
        if event_state(it, *evg).is_some_and(|s| s.stopped_now) {
            break;
        }
        let et = it.get(&e, "t")?;
        if it.to_str(&et)?.as_ref() != ty {
            continue;
        }
        let cap = Interp::truthy(&it.get(&e, "c")?);
        if let Some(want) = capture {
            if cap != want {
                continue;
            }
        }
        // Removed by an earlier listener?
        if Interp::truthy(&it.get(&e, "gone")?) {
            continue;
        }
        if Interp::truthy(&it.get(&e, "o")?) {
            it.put(&e, "gone", Value::Bool(true))?;
            if let Kind::Array(v) = &mut it.heap.get_mut(list).kind {
                v.retain(|x| !Interp::strict_eq(x, &e));
            }
        }
        let f = it.get(&e, "f")?;
        let r = if it.is_callable(&f) {
            it.call(&f, cur.clone(), vec![ev.clone()])
        } else {
            let he = it.get(&f, "handleEvent")?;
            if it.is_callable(&he) {
                it.call(&he, f.clone(), vec![ev.clone()])
            } else {
                Ok(Value::Undef)
            }
        };
        match r {
            Ok(_) => {}
            Err(Ctrl::Abort(m)) => return Err(Ctrl::Abort(m)),
            Err(e) => report(it, e),
        }
    }
    Ok(())
}

/// `cur.on<type>`: a function property, else the element's attribute
/// (window handlers like onload also come from <body>).
fn handler(it: &mut Interp, cur: &Value, ty: &str) -> R<Option<Value>> {
    let Value::Obj(g) = cur else { return Ok(None) };
    let key = format!("on{ty}");
    if let Some(Slot::Data(v, _)) = it.heap.get(*g).props.get(&key) {
        let v = v.clone();
        return Ok(if it.is_callable(&v) { Some(v) } else { None });
    }
    let n = match it.heap.get(*g).kind {
        Kind::Node(n) => Some(n),
        _ if *g == it.global && matches!(ty, "load" | "unload" | "beforeunload" | "resize" | "scroll" | "hashchange" | "popstate" | "pageshow") => it.host.doc.body(),
        _ => None,
    };
    let Some(n) = n else { return Ok(None) };
    let Some(src) = attr(it, n, &key) else { return Ok(None) };
    // Compiled once per source text.
    let cache_key = format!("@@{key}");
    if let Some(Slot::Data(c, _)) = it.heap.get(*g).props.get(&cache_key) {
        let c = c.clone();
        if let (Some(Slot::Data(Value::Str(old), _)), Value::Obj(_)) = (it.heap.get(*g).props.get("@@src:".to_string().as_str()), &c) {
            let _ = old;
        }
        if let Value::Obj(cg) = &c {
            if let Some(Slot::Data(Value::Str(was), _)) = it.heap.get(*cg).props.get("@@source") {
                if was.as_ref() == src {
                    return Ok(Some(c));
                }
            }
        }
    }
    let wrapped = format!("(function (event) {{\n{src}\n}})");
    let prog = match super::parser::parse_program(&wrapped) {
        Ok(p) => p,
        Err(e) => {
            it.host.error(format!("{e} (in an {key} attribute)"));
            return Ok(None);
        }
    };
    let Some(super::ast::Stmt::Expr(super::ast::Expr::Func(f))) = prog.body.first() else { return Ok(None) };
    let saved = it.env;
    it.env = it.genv;
    let fv = it.make_closure(f, CtorKind::None, None);
    it.env = saved;
    if let Value::Obj(fg) = &fv {
        it.hide(*fg, "@@source", Value::str(&src));
    }
    it.hide(*g, &cache_key, fv.clone());
    Ok(Some(fv))
}

/// A plain event (load, error, change...) at node `n`.
pub fn fire_simple(it: &mut Interp, n: NodeId, ty: &str) -> R<bool> {
    let bubbles = matches!(ty, "change" | "input" | "submit" | "reset");
    let ev = new_event(it, ty, &EventInit { bubbles, cancelable: ty == "submit", ..Default::default() });
    let t = wrap(it, n);
    dispatch(it, &t, ev)
}

fn listeners_of(it: &mut Interp, o: Gc) -> Gc {
    if let Some(Slot::Data(Value::Obj(l), _)) = it.heap.get(o).props.get("@@listeners") {
        return *l;
    }
    let l = it.new_array(Vec::new());
    it.hide(o, "@@listeners", l.clone());
    l.as_obj().unwrap()
}

fn capture_flag(it: &mut Interp, v: &Value) -> R<(bool, bool)> {
    match v {
        Value::Obj(_) => {
            let c = it.get(v, "capture")?;
            let o = it.get(v, "once")?;
            Ok((Interp::truthy(&c), Interp::truthy(&o)))
        }
        other => Ok((Interp::truthy(other), false)),
    }
}

fn add_event_listener(it: &mut Interp, a: &A) -> R<Value> {
    let Value::Obj(o) = a.this else { return Ok(Value::Undef) };
    let ty = it.to_str(&a.get(0))?;
    let f = a.get(1);
    if f.is_nullish() {
        return Ok(Value::Undef);
    }
    let (cap, once) = capture_flag(it, &a.get(2))?;
    let list = listeners_of(it, o);
    let len = match &it.heap.get(list).kind {
        Kind::Array(v) => v.len(),
        _ => 0,
    };
    if len > 10_000 {
        return Err(it.err(ErrKind::Range, "too many event listeners"));
    }
    // The same (type, listener, capture) is only added once.
    let existing: Vec<Value> = match &it.heap.get(list).kind {
        Kind::Array(v) => v.clone(),
        _ => Vec::new(),
    };
    for e in existing {
        let (et, ef, ec) = (it.get(&e, "t")?, it.get(&e, "f")?, it.get(&e, "c")?);
        if matches!(&et, Value::Str(x) if *x == ty) && Interp::strict_eq(&ef, &f) && Interp::truthy(&ec) == cap {
            return Ok(Value::Undef);
        }
    }
    let e = it.new_object();
    it.set_prop(e, "t", Value::Str(ty));
    it.set_prop(e, "f", f);
    it.set_prop(e, "c", Value::Bool(cap));
    it.set_prop(e, "o", Value::Bool(once));
    if let Kind::Array(v) = &mut it.heap.get_mut(list).kind {
        v.push(Value::Obj(e));
    }
    Ok(Value::Undef)
}

fn remove_event_listener(it: &mut Interp, a: &A) -> R<Value> {
    let Value::Obj(o) = a.this else { return Ok(Value::Undef) };
    let ty = it.to_str(&a.get(0))?;
    let f = a.get(1);
    let (cap, _) = capture_flag(it, &a.get(2))?;
    let list = listeners_of(it, o);
    let entries: Vec<Value> = match &it.heap.get(list).kind {
        Kind::Array(v) => v.clone(),
        _ => return Ok(Value::Undef),
    };
    let mut keep = Vec::new();
    for e in entries {
        let (et, ef, ec) = (it.get(&e, "t")?, it.get(&e, "f")?, it.get(&e, "c")?);
        if matches!(&et, Value::Str(x) if *x == ty) && Interp::strict_eq(&ef, &f) && Interp::truthy(&ec) == cap {
            it.put(&e, "gone", Value::Bool(true))?;
        } else {
            keep.push(e);
        }
    }
    if let Kind::Array(v) = &mut it.heap.get_mut(list).kind {
        *v = keep;
    }
    Ok(Value::Undef)
}

fn dispatch_event(it: &mut Interp, a: &A) -> R<Value> {
    let ev = a.get(0);
    let Value::Obj(eg) = ev else { return it.type_err("dispatchEvent: not an event") };
    if event_state(it, eg).is_none() {
        return it.type_err("dispatchEvent: not an event");
    }
    it.set_prop(eg, "isTrusted", Value::Bool(false));
    with_state(it, eg, |s| {
        s.stopped = false;
        s.stopped_now = false;
    });
    let ok = dispatch(it, &a.this, eg)?;
    Ok(Value::Bool(ok))
}

/// addEventListener & co. on an object (nodes, window, XMLHttpRequest...).
pub fn install_event_target(it: &mut Interp, o: Gc) {
    it.method(o, "addEventListener", add_event_listener, 2);
    it.method(o, "removeEventListener", remove_event_listener, 2);
    it.method(o, "dispatchEvent", dispatch_event, 1);
}

fn event_ctor(it: &mut Interp, a: &A) -> R<Value> {
    if a.new_target.is_none() {
        return it.type_err("Failed to construct 'Event': use 'new'");
    }
    let ty = it.to_str(&a.get(0))?.to_string();
    let init = a.get(1);
    let mut ei = EventInit::default();
    if let Value::Obj(_) = init {
        ei.bubbles = Interp::truthy(&it.get(&init, "bubbles")?);
        ei.cancelable = Interp::truthy(&it.get(&init, "cancelable")?);
    }
    let g = new_event(it, &ty, &ei);
    it.set_prop(g, "isTrusted", Value::Bool(false));
    if let Value::Obj(ig) = &init {
        let keys = it.own_keys(*ig, true)?;
        for k in keys {
            if matches!(k.as_ref(), "bubbles" | "cancelable") {
                continue;
            }
            let v = it.get(&init, &k)?;
            it.set_prop(g, &k, v);
        }
    }
    // Subclass prototypes (class X extends Event).
    if let Some(Value::Obj(nt)) = &a.new_target {
        let p = it.get(&Value::Obj(*nt), "prototype")?;
        if let Value::Obj(pg) = p {
            it.heap.get_mut(g).proto = Some(pg);
        }
    }
    Ok(Value::Obj(g))
}

fn install_events(it: &mut Interp) {
    let proto = it.new_object();
    it.p.event = Some(proto);
    it.method(proto, "preventDefault", |it, a| {
        if let Value::Obj(g) = a.this {
            with_state(it, g, |s| {
                if s.cancelable {
                    s.prevented = true
                }
            });
        }
        Ok(Value::Undef)
    }, 0);
    it.method(proto, "stopPropagation", |it, a| {
        if let Value::Obj(g) = a.this {
            with_state(it, g, |s| s.stopped = true);
        }
        Ok(Value::Undef)
    }, 0);
    it.method(proto, "stopImmediatePropagation", |it, a| {
        if let Value::Obj(g) = a.this {
            with_state(it, g, |s| {
                s.stopped = true;
                s.stopped_now = true;
            });
        }
        Ok(Value::Undef)
    }, 0);
    it.method(proto, "composedPath", |it, a| {
        let p = it.get(&a.this, "@@path")?;
        if p.is_nullish() {
            return Ok(it.new_array(Vec::new()));
        }
        let items = it.collect(&p)?;
        Ok(it.new_array(items))
    }, 0);
    it.method(proto, "initEvent", |it, a| {
        if let Value::Obj(g) = a.this {
            let t = a.get(0);
            it.set_prop(g, "type", t);
            it.set_prop(g, "bubbles", Value::Bool(Interp::truthy(&a.get(1))));
            let c = Interp::truthy(&a.get(2));
            it.set_prop(g, "cancelable", Value::Bool(c));
            with_state(it, g, |s| s.cancelable = c);
        }
        Ok(Value::Undef)
    }, 3);
    it.getter(proto, "defaultPrevented", |it, a| {
        Ok(Value::Bool(match a.this {
            Value::Obj(g) => event_state(it, g).is_some_and(|s| s.prevented),
            _ => false,
        }))
    });
    it.getter(proto, "srcElement", |it, a| it.get(&a.this, "target"));
    accessor(it, proto, "returnValue", |it, a| {
        Ok(Value::Bool(match a.this {
            Value::Obj(g) => !event_state(it, g).is_some_and(|s| s.prevented),
            _ => true,
        }))
    }, |it, a| {
        if let Value::Obj(g) = a.this {
            if !Interp::truthy(&a.get(0)) {
                with_state(it, g, |s| {
                    if s.cancelable {
                        s.prevented = true
                    }
                });
            }
        }
        Ok(Value::Undef)
    });
    accessor(it, proto, "cancelBubble", |it, a| {
        Ok(Value::Bool(match a.this {
            Value::Obj(g) => event_state(it, g).is_some_and(|s| s.stopped),
            _ => false,
        }))
    }, |it, a| {
        if let Value::Obj(g) = a.this {
            if Interp::truthy(&a.get(0)) {
                with_state(it, g, |s| s.stopped = true);
            }
        }
        Ok(Value::Undef)
    });
    for (k, v) in [("NONE", 0.0), ("CAPTURING_PHASE", 1.0), ("AT_TARGET", 2.0), ("BUBBLING_PHASE", 3.0)] {
        it.hide(proto, k, Value::Num(v));
    }
    let g = it.global;
    for name in ["Event", "CustomEvent", "UIEvent", "MouseEvent", "PointerEvent", "KeyboardEvent", "FocusEvent", "InputEvent", "WheelEvent", "TouchEvent", "ErrorEvent", "MessageEvent", "PopStateEvent", "HashChangeEvent", "ProgressEvent", "SubmitEvent", "AnimationEvent", "TransitionEvent", "StorageEvent"] {
        let c = it.new_native(name, event_ctor, 1);
        if let Kind::Native(n) = &mut it.heap.get_mut(c).kind {
            n.ctor = true;
        }
        it.heap.get_mut(c).props.set("prototype".into(), Slot::Data(Value::Obj(proto), 0));
        it.hide(g, name, Value::Obj(c));
        if name == "Event" {
            it.hide(proto, "constructor", Value::Obj(c));
        }
    }
    // EventTarget: a base class scripts can extend.
    let et_proto = it.new_object();
    install_event_target(it, et_proto);
    let c = it.new_native("EventTarget", |it, a| {
        let o = it.new_object();
        if let Some(Value::Obj(nt)) = &a.new_target {
            let p = it.get(&Value::Obj(*nt), "prototype")?;
            if let Value::Obj(pg) = p {
                it.heap.get_mut(o).proto = Some(pg);
            }
        }
        Ok(Value::Obj(o))
    }, 0);
    if let Kind::Native(n) = &mut it.heap.get_mut(c).kind {
        n.ctor = true;
    }
    it.heap.get_mut(c).props.set("prototype".into(), Slot::Data(Value::Obj(et_proto), 0));
    it.hide(et_proto, "constructor", Value::Obj(c));
    it.hide(g, "EventTarget", Value::Obj(c));
}

pub fn accessor(it: &mut Interp, o: Gc, name: &str, get: NativeFn, set: NativeFn) {
    let gf = it.new_native(name, get, 0);
    let sf = it.new_native(name, set, 1);
    it.heap.get_mut(o).props.set(name.into(), Slot::Accessor(Some(gf), Some(sf), F_CONF));
}

// ---- selectors -----------------------------------------------------------------------

fn selectors(it: &mut Interp, src: &str) -> R<Rc<Vec<Selector>>> {
    if let Some(c) = it.host.selectors.get(src) {
        return match c {
            Some(s) => Ok(s.clone()),
            None => Err(it.err(ErrKind::Syntax, &format!("'{src}' is not a valid selector"))),
        };
    }
    let cvs = crate::css::tokenizer::component_values(crate::css::tokenizer::tokenize(src));
    let parsed = crate::css::parser::parse_selector_list(&cvs).filter(|l| l.iter().all(|s| s.pseudo_element.is_none())).map(Rc::new);
    if it.host.selectors.len() > 2000 {
        it.host.selectors.clear();
    }
    it.host.selectors.insert(src.to_string(), parsed.clone());
    match parsed {
        Some(s) => Ok(s),
        None => Err(it.err(ErrKind::Syntax, &format!("'{src}' is not a valid selector"))),
    }
}

fn matches_any(doc: &Document, n: NodeId, sels: &[Selector]) -> bool {
    doc.element(n).is_some() && sels.iter().any(|s| crate::css::matching::matches(doc, n, s))
}

fn query(it: &mut Interp, root: NodeId, sel: &str, all: bool) -> R<Vec<NodeId>> {
    let sels = selectors(it, sel)?;
    let mut out = Vec::new();
    let nodes = it.host.doc.descendants(root);
    for n in nodes.into_iter().skip(1) {
        it.tick()?;
        if matches_any(&it.host.doc, n, &sels) {
            out.push(n);
            if !all {
                break;
            }
        }
    }
    Ok(out)
}

fn elements_where(it: &mut Interp, root: NodeId, f: impl Fn(&Element) -> bool) -> R<Vec<NodeId>> {
    let mut out = Vec::new();
    for n in it.host.doc.descendants(root).into_iter().skip(1) {
        if let Some(e) = it.host.doc.element(n) {
            if f(e) {
                out.push(n);
            }
        }
    }
    it.steps += out.len() as u64;
    Ok(out)
}

fn node_list(it: &mut Interp, ns: Vec<NodeId>) -> Value {
    let v: Vec<Value> = ns.into_iter().map(|n| wrap(it, n)).collect();
    it.new_array(v)
}

pub fn get_element_by_id(doc: &Document, id: &str) -> Option<NodeId> {
    doc.descendants(Document::ROOT).into_iter().find(|&n| doc.element(n).is_some_and(|e| e.id() == Some(id)))
}

// ---- markup -------------------------------------------------------------------------

const VOID: &[&str] = &["area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source", "track", "wbr", "param", "keygen"];

fn escape_text(s: &str, out: &mut String, attr: bool) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' if !attr => out.push_str("&lt;"),
            '>' if !attr => out.push_str("&gt;"),
            '"' if attr => out.push_str("&quot;"),
            '\u{A0}' => out.push_str("&nbsp;"),
            c => out.push(c),
        }
    }
}

/// The HTML for `n` (or only its children).
pub fn serialize(doc: &Document, n: NodeId, inner: bool) -> String {
    let mut out = String::new();
    // An explicit stack: (node, closing?)
    let mut stack: Vec<(NodeId, bool)> = Vec::new();
    if inner {
        for &c in doc.children(n).iter().rev() {
            stack.push((c, false));
        }
    } else {
        stack.push((n, false));
    }
    while let Some((id, close)) = stack.pop() {
        if out.len() > 64 << 20 {
            break;
        }
        match &doc.nodes[id].data {
            NodeData::Element(e) => {
                if close {
                    out.push_str("</");
                    out.push_str(&e.name);
                    out.push('>');
                    continue;
                }
                if e.name == FRAGMENT {
                    for &c in doc.children(id).iter().rev() {
                        stack.push((c, false));
                    }
                    continue;
                }
                out.push('<');
                out.push_str(&e.name);
                for (k, v) in &e.attrs {
                    out.push(' ');
                    out.push_str(k);
                    out.push_str("=\"");
                    escape_text(v, &mut out, true);
                    out.push('"');
                }
                out.push('>');
                if VOID.contains(&e.name.as_str()) {
                    continue;
                }
                stack.push((id, true));
                for &c in doc.children(id).iter().rev() {
                    stack.push((c, false));
                }
            }
            NodeData::Text(t) => {
                let raw = doc.parent(id).is_some_and(|p| matches!(doc.name(p), "script" | "style" | "xmp" | "noscript" | "iframe" | "noembed" | "noframes" | "plaintext"));
                if raw {
                    out.push_str(t);
                } else {
                    escape_text(t, &mut out, false);
                }
            }
            NodeData::Comment(c) => {
                out.push_str("<!--");
                out.push_str(c);
                out.push_str("-->");
            }
            NodeData::Doctype(d) => {
                out.push_str("<!DOCTYPE ");
                out.push_str(d);
                out.push('>');
            }
            NodeData::Document => {
                for &c in doc.children(id).iter().rev() {
                    stack.push((c, false));
                }
            }
        }
    }
    out
}

/// Parses markup as the children of an element named `context`, into
/// detached nodes of the page's document (scripts in it never run).
fn parse_fragment(it: &mut Interp, context: &str, html: &str) -> R<Vec<NodeId>> {
    if html.len() > 16 << 20 {
        return Err(it.err(ErrKind::Range, "markup too long"));
    }
    let (prefix, path): (&str, &[&str]) = match context {
        "table" => ("<table>", &["table"]),
        "tbody" | "thead" | "tfoot" => ("<table><tbody>", &["table", "tbody"]),
        "tr" => ("<table><tbody><tr>", &["table", "tbody", "tr"]),
        "select" => ("<select>", &["select"]),
        "head" => ("", &[]),
        _ => ("", &[]),
    };
    let src = crate::html::parse(&format!("<!doctype html><body>{prefix}{html}"));
    let mut at = match src.body() {
        Some(b) => b,
        None => return Ok(Vec::new()),
    };
    for name in path {
        match src.child_elements(at).find(|&c| src.is(c, name)) {
            Some(c) => at = c,
            None => break,
        }
    }
    let mut out = Vec::new();
    for &c in src.children(at) {
        out.push(import(it, &src, c)?);
    }
    Ok(out)
}

/// Copies node `n` of `src` (deeply) into the page's document.
fn import(it: &mut Interp, src: &Document, n: NodeId) -> R<NodeId> {
    if it.host.doc.nodes.len() + src.nodes.len() >= MAX_NODES {
        return Err(it.err(ErrKind::Range, "the page has too many nodes"));
    }
    let root = it.host.doc.new_node(src.nodes[n].data.clone());
    let mut stack = vec![(n, root)];
    while let Some((s, d)) = stack.pop() {
        for &c in src.children(s) {
            let nd = it.host.doc.new_node(src.nodes[c].data.clone());
            it.host.doc.append(d, nd);
            stack.push((c, nd));
        }
    }
    for d in it.host.doc.descendants(root) {
        if it.host.doc.is(d, "script") {
            it.host.started.insert(d);
        }
    }
    Ok(root)
}

fn clone_node(it: &mut Interp, n: NodeId, deep: bool) -> R<NodeId> {
    if it.host.doc.nodes.len() >= MAX_NODES {
        return Err(it.err(ErrKind::Range, "the page has too many nodes"));
    }
    let data = it.host.doc.nodes[n].data.clone();
    let root = it.host.doc.new_node(if matches!(data, NodeData::Document) { NodeData::Element(Element { name: FRAGMENT.into(), attrs: Vec::new(), foreign: false }) } else { data });
    if deep {
        let mut stack = vec![(n, root)];
        while let Some((s, d)) = stack.pop() {
            let kids = it.host.doc.children(s).to_vec();
            if it.host.doc.nodes.len() + kids.len() >= MAX_NODES {
                return Err(it.err(ErrKind::Range, "the page has too many nodes"));
            }
            for c in kids {
                let nd = it.host.doc.new_node(it.host.doc.nodes[c].data.clone());
                it.host.doc.append(d, nd);
                stack.push((c, nd));
            }
        }
    }
    // Copies of scripts that ran don't run again.
    for d in it.host.doc.descendants(root) {
        if it.host.doc.is(d, "script") {
            let src_started = true;
            if src_started {
                it.host.started.insert(d);
            }
        }
    }
    Ok(root)
}

fn set_inner_html(it: &mut Interp, n: NodeId, html: &str) -> R<()> {
    let ctx = it.host.doc.name(n).to_string();
    let nodes = parse_fragment(it, &ctx, html)?;
    remove_children(it, n);
    for c in nodes {
        it.host.doc.append(n, c);
    }
    it.host.dirty = true;
    Ok(())
}

fn set_text(it: &mut Interp, n: NodeId, text: &str) -> R<()> {
    match &mut it.host.doc.nodes[n].data {
        NodeData::Text(t) | NodeData::Comment(t) => {
            *t = text.to_string();
            it.host.dirty = true;
            return Ok(());
        }
        _ => {}
    }
    remove_children(it, n);
    if !text.is_empty() {
        let t = new_node(it, NodeData::Text(text.to_string()))?;
        it.host.doc.append(n, t);
    }
    if is_el(it, n, "textarea") {
        it.host.form.values.insert(n, text.to_string());
    }
    Ok(())
}

/// Nodes from arguments: nodes as they are, anything else as text.
fn nodes_from_args(it: &mut Interp, a: &A) -> R<Vec<NodeId>> {
    let mut out = Vec::new();
    for v in a.args {
        match node_of(it, v) {
            Some(n) => out.push(n),
            None => {
                let t = it.to_str(v)?.to_string();
                out.push(new_node(it, NodeData::Text(t))?);
            }
        }
    }
    Ok(out)
}

// ---- forms -------------------------------------------------------------------------

fn input_type(it: &Interp, n: NodeId) -> String {
    attr(it, n, "type").map(|t| t.trim().to_ascii_lowercase()).filter(|t| !t.is_empty()).unwrap_or_else(|| "text".into())
}

fn options_of(it: &Interp, sel: NodeId) -> Vec<NodeId> {
    it.host.doc.descendants(sel).into_iter().filter(|&o| it.host.doc.is(o, "option")).collect()
}

fn option_value(it: &Interp, o: NodeId) -> String {
    attr(it, o, "value").unwrap_or_else(|| it.host.doc.text_content(o).split_whitespace().collect::<Vec<_>>().join(" "))
}

fn selected_index(it: &Interp, sel: NodeId) -> Option<usize> {
    let opts = options_of(it, sel);
    opts.iter().position(|&o| attr(it, o, "selected").is_some()).or(if opts.is_empty() { None } else { Some(0) })
}

fn select_index(it: &mut Interp, sel: NodeId, i: Option<usize>) {
    let opts = options_of(it, sel);
    for (k, o) in opts.iter().enumerate() {
        if Some(k) == i {
            set_attr(it, *o, "selected", "");
        } else {
            remove_attr(it, *o, "selected");
        }
    }
}

pub fn get_value(it: &Interp, n: NodeId) -> String {
    let name = it.host.doc.name(n);
    match name {
        "input" => {
            let t = input_type(it, n);
            if t == "checkbox" || t == "radio" {
                return attr(it, n, "value").unwrap_or_else(|| "on".into());
            }
            it.host.form.values.get(&n).cloned().or_else(|| attr(it, n, "value")).unwrap_or_default()
        }
        "textarea" => it.host.form.values.get(&n).cloned().unwrap_or_else(|| it.host.doc.text_content(n)),
        "select" => selected_index(it, n).and_then(|i| options_of(it, n).get(i).copied()).map(|o| option_value(it, o)).unwrap_or_default(),
        "option" => option_value(it, n),
        _ => attr(it, n, "value").unwrap_or_default(),
    }
}

fn set_value(it: &mut Interp, n: NodeId, v: &str) {
    let name = it.host.doc.name(n).to_string();
    match name.as_str() {
        "input" => {
            let t = input_type(it, n);
            if t == "checkbox" || t == "radio" || t == "hidden" || t == "submit" || t == "button" || t == "reset" {
                set_attr(it, n, "value", v);
            } else {
                it.host.form.values.insert(n, v.chars().filter(|c| *c != '\n' && *c != '\r').take(1 << 16).collect());
                it.host.repaint = true;
            }
        }
        "textarea" => {
            it.host.form.values.insert(n, v.chars().take(1 << 20).collect());
            it.host.repaint = true;
        }
        "select" => {
            let opts = options_of(it, n);
            let i = opts.iter().position(|&o| option_value(it, o) == v);
            select_index(it, n, i);
        }
        _ => set_attr(it, n, "value", v),
    }
}

fn is_checkable(it: &Interp, n: NodeId) -> bool {
    is_el(it, n, "input") && matches!(input_type(it, n).as_str(), "checkbox" | "radio")
}

fn set_checked(it: &mut Interp, n: NodeId, on: bool) {
    if on && input_type(it, n) == "radio" {
        if let Some(name) = attr(it, n, "name") {
            let group: Vec<NodeId> = (0..it.host.doc.nodes.len()).filter(|&o| o != n && is_el(it, o, "input") && attr(it, o, "name").as_deref() == Some(name.as_str()) && input_type(it, o) == "radio").collect();
            for g in group {
                it.host.form.checked.insert(g, false);
            }
        }
    }
    it.host.form.checked.insert(n, on);
    it.host.repaint = true;
}

fn form_of(it: &Interp, n: NodeId) -> Option<NodeId> {
    if let Some(id) = attr(it, n, "form") {
        if let Some(f) = get_element_by_id(&it.host.doc, &id) {
            return Some(f);
        }
    }
    let mut p = it.host.doc.parent(n);
    while let Some(x) = p {
        if is_el(it, x, "form") {
            return Some(x);
        }
        p = it.host.doc.parent(x);
    }
    None
}

fn form_controls(it: &Interp, f: NodeId) -> Vec<NodeId> {
    it.host.doc.descendants(f).into_iter().filter(|&n| matches!(it.host.doc.name(n), "input" | "select" | "textarea" | "button" | "fieldset" | "output")).collect()
}

// ---- style="" ----------------------------------------------------------------------

/// The declarations of a style attribute, in order (name lower-case).
fn style_decls(text: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut cur = String::new();
    let mut parts = Vec::new();
    for c in text.chars() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
                cur.push(c);
            }
            None => match c {
                '"' | '\'' => {
                    quote = Some(c);
                    cur.push(c);
                }
                '(' => {
                    depth += 1;
                    cur.push(c);
                }
                ')' => {
                    depth -= 1;
                    cur.push(c);
                }
                ';' if depth <= 0 => parts.push(std::mem::take(&mut cur)),
                c => cur.push(c),
            },
        }
    }
    parts.push(cur);
    for p in parts {
        if let Some((k, v)) = p.split_once(':') {
            let k = k.trim().to_ascii_lowercase();
            let v = v.trim().to_string();
            if k.is_empty() {
                continue;
            }
            out.retain(|(x, _)| *x != k);
            out.push((k, v));
        }
    }
    out
}

fn style_text(decls: &[(String, String)]) -> String {
    decls.iter().map(|(k, v)| format!("{k}: {v};")).collect::<Vec<_>>().join(" ")
}

/// backgroundColor -> background-color, cssFloat -> float, WebkitX -> -webkit-x.
fn to_kebab(k: &str) -> String {
    if k == "cssFloat" {
        return "float".into();
    }
    if k.contains('-') {
        return k.to_ascii_lowercase();
    }
    let mut o = String::new();
    for (i, c) in k.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 || matches!(k, _ if k.starts_with("Webkit") || k.starts_with("Moz") || k.starts_with("Ms")) {
                o.push('-');
            }
            o.push(c.to_ascii_lowercase());
        } else {
            o.push(c);
        }
    }
    o
}

fn to_camel(k: &str) -> String {
    if k == "float" {
        return "cssFloat".into();
    }
    let mut o = String::new();
    let mut up = false;
    for (i, c) in k.chars().enumerate() {
        if c == '-' {
            up = i > 0;
            continue;
        }
        if up {
            o.push(c.to_ascii_uppercase());
            up = false;
        } else {
            o.push(c);
        }
    }
    o
}

fn get_style(it: &Interp, n: NodeId, prop: &str) -> String {
    let decls = style_decls(&attr(it, n, "style").unwrap_or_default());
    decls.into_iter().find(|(k, _)| k == prop).map(|(_, v)| v).unwrap_or_default()
}

fn set_style(it: &mut Interp, n: NodeId, prop: &str, v: &str) {
    let mut decls = style_decls(&attr(it, n, "style").unwrap_or_default());
    let v = v.trim();
    if v.is_empty() {
        decls.retain(|(k, _)| k != prop);
    } else {
        match decls.iter_mut().find(|(k, _)| k == prop) {
            Some(d) => d.1 = v.to_string(),
            None => decls.push((prop.to_string(), v.to_string())),
        }
    }
    let t = style_text(&decls);
    set_attr(it, n, "style", &t);
}

/// Is `key` found on `g`'s prototype chain (a method, not a host property)?
fn on_proto(it: &Interp, g: Gc, key: &str) -> bool {
    let mut cur = it.heap.get(g).proto;
    let mut hops = 0;
    while let Some(p) = cur {
        if it.heap.get(p).props.get(key).is_some() {
            return true;
        }
        cur = it.heap.get(p).proto;
        hops += 1;
        if hops > 100 {
            break;
        }
    }
    false
}

fn css_name(key: &str) -> bool {
    !key.is_empty() && !key.starts_with("@@") && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') && !key.chars().next().unwrap().is_ascii_digit()
}

fn data_attr(key: &str) -> String {
    let mut o = String::from("data-");
    for c in key.chars() {
        if c.is_ascii_uppercase() {
            o.push('-');
            o.push(c.to_ascii_lowercase());
        } else {
            o.push(c);
        }
    }
    o
}

// ---- the host hooks (called by the interpreter) -------------------------------------

/// Reads a host property of a node-backed object; None: an ordinary property.
pub fn host_get(it: &mut Interp, g: Gc, key: &str) -> R<Option<Value>> {
    let kind = it.heap.get(g).kind.clone();
    match kind {
        Kind::Node(n) if n < it.host.doc.nodes.len() => node_get(it, n, key),
        Kind::ClassList(n) if n < it.host.doc.nodes.len() => {
            let classes = class_list(it, n);
            Ok(match key {
                "length" => Some(Value::Num(classes.len() as f64)),
                "value" => Some(s(&attr(it, n, "class").unwrap_or_default())),
                k => super::value::index_of_key(k).map(|i| classes.get(i).map(|c| s(c)).unwrap_or(Value::Undef)),
            })
        }
        Kind::Style(n) if n < it.host.doc.nodes.len() => {
            if key == "cssText" {
                return Ok(Some(s(&attr(it, n, "style").unwrap_or_default())));
            }
            if key == "length" {
                return Ok(Some(Value::Num(style_decls(&attr(it, n, "style").unwrap_or_default()).len() as f64)));
            }
            if let Some(i) = super::value::index_of_key(key) {
                let d = style_decls(&attr(it, n, "style").unwrap_or_default());
                return Ok(Some(d.get(i).map(|(k, _)| s(k)).unwrap_or(Value::Undef)));
            }
            if !css_name(key) || on_proto(it, g, key) || it.heap.get(g).props.get(key).is_some() {
                return Ok(None);
            }
            Ok(Some(s(&get_style(it, n, &to_kebab(key)))))
        }
        Kind::Dataset(n) if n < it.host.doc.nodes.len() => {
            if key.starts_with("@@") {
                return Ok(None);
            }
            Ok(attr(it, n, &data_attr(key)).map(|v| s(&v)))
        }
        _ => Ok(None),
    }
}

/// Writes a host property; false: store it as an ordinary property.
pub fn host_set(it: &mut Interp, g: Gc, key: &str, v: &Value) -> R<bool> {
    let kind = it.heap.get(g).kind.clone();
    match kind {
        Kind::Node(n) if n < it.host.doc.nodes.len() => node_set(it, n, key, v),
        Kind::ClassList(n) => {
            if key == "value" {
                let t = it.to_str(v)?.to_string();
                set_attr(it, n, "class", &t);
                return Ok(true);
            }
            Ok(false)
        }
        Kind::Style(n) => {
            if key == "cssText" {
                let t = it.to_str(v)?.to_string();
                set_attr(it, n, "style", &t);
                return Ok(true);
            }
            if !css_name(key) || on_proto(it, g, key) {
                return Ok(false);
            }
            let t = if v.is_nullish() { String::new() } else { it.to_str(v)?.to_string() };
            set_style(it, n, &to_kebab(key), &t);
            Ok(true)
        }
        Kind::Dataset(n) => {
            if key.starts_with("@@") {
                return Ok(false);
            }
            let t = it.to_str(v)?.to_string();
            set_attr(it, n, &data_attr(key), &t);
            Ok(true)
        }
        _ => Ok(false),
    }
}

pub fn host_keys(it: &mut Interp, g: Gc) -> Vec<JsStr> {
    match it.heap.get(g).kind.clone() {
        Kind::Dataset(n) => el(it, n)
            .map(|e| e.attrs.iter().filter_map(|(k, _)| k.strip_prefix("data-")).map(|k| JsStr::from(to_camel(k))).collect())
            .unwrap_or_default(),
        Kind::Style(n) => style_decls(&attr(it, n, "style").unwrap_or_default()).into_iter().map(|(k, _)| JsStr::from(to_camel(&k))).collect(),
        _ => Vec::new(),
    }
}

fn class_list(it: &Interp, n: NodeId) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for c in attr(it, n, "class").unwrap_or_default().split_ascii_whitespace() {
        if !v.iter().any(|x| x == c) {
            v.push(c.to_string());
        }
    }
    v
}

fn sub_object(it: &mut Interp, n: NodeId, which: &str) -> Value {
    let key = format!("@@{which}");
    let w = wrap(it, n);
    let Value::Obj(wg) = w else { return Value::Undef };
    if let Some(Slot::Data(v, _)) = it.heap.get(wg).props.get(&key) {
        return v.clone();
    }
    let (proto, kind) = match which {
        "classList" => (it.p.class_list, Kind::ClassList(n)),
        "style" => (it.p.style, Kind::Style(n)),
        _ => (it.p.dataset, Kind::Dataset(n)),
    };
    let o = Value::Obj(it.alloc(Obj::new(proto, kind)));
    it.hide(wg, &key, o.clone());
    o
}

fn resolved_attr(it: &Interp, n: NodeId, name: &str) -> String {
    match attr(it, n, name) {
        Some(v) => it.host.resolve(&v).map(|u| u.as_str().to_string()).unwrap_or(v),
        None => String::new(),
    }
}

fn rect_of(it: &Interp, n: NodeId) -> [f32; 4] {
    it.host.rects.get(&n).copied().unwrap_or([0.0; 4])
}

fn rect_object(it: &mut Interp, r: [f32; 4]) -> Value {
    let o = it.new_object();
    let (x, y) = (r[0] - it.host.scroll.0, r[1] - it.host.scroll.1);
    for (k, v) in [("x", x), ("y", y), ("left", x), ("top", y), ("width", r[2]), ("height", r[3]), ("right", x + r[2]), ("bottom", y + r[3])] {
        it.set_prop(o, k, Value::Num(v as f64));
    }
    Value::Obj(o)
}

fn node_type(doc: &Document, n: NodeId) -> f64 {
    match &doc.nodes[n].data {
        NodeData::Element(e) if e.name == FRAGMENT => 11.0,
        NodeData::Element(_) => 1.0,
        NodeData::Text(_) => 3.0,
        NodeData::Comment(_) => 8.0,
        NodeData::Document => 9.0,
        NodeData::Doctype(_) => 10.0,
    }
}

fn element_children(it: &Interp, n: NodeId) -> Vec<NodeId> {
    it.host.doc.children(n).iter().copied().filter(|&c| it.host.doc.element(c).is_some()).collect()
}

fn sibling(it: &Interp, n: NodeId, dir: i32, elements: bool) -> Option<NodeId> {
    let p = it.host.doc.parent(n)?;
    let sibs = it.host.doc.children(p);
    let i = sibs.iter().position(|&c| c == n)? as i64;
    let mut j = i + dir as i64;
    while j >= 0 && (j as usize) < sibs.len() {
        let c = sibs[j as usize];
        if !elements || it.host.doc.element(c).is_some() {
            return Some(c);
        }
        j += dir as i64;
    }
    None
}

const BOOL_ATTRS: &[(&str, &str)] = &[
    ("disabled", "disabled"),
    ("hidden", "hidden"),
    ("required", "required"),
    ("readOnly", "readonly"),
    ("multiple", "multiple"),
    ("autofocus", "autofocus"),
    ("noValidate", "novalidate"),
    ("async", "async"),
    ("defer", "defer"),
    ("open", "open"),
    ("defaultChecked", "checked"),
    ("noModule", "nomodule"),
    ("controls", "controls"),
    ("autoplay", "autoplay"),
    ("loop", "loop"),
    ("muted", "muted"),
    ("inert", "inert"),
];

const STR_ATTRS: &[(&str, &str)] = &[
    ("id", "id"),
    ("className", "class"),
    ("title", "title"),
    ("lang", "lang"),
    ("dir", "dir"),
    ("name", "name"),
    ("placeholder", "placeholder"),
    ("alt", "alt"),
    ("rel", "rel"),
    ("target", "target"),
    ("method", "method"),
    ("htmlFor", "for"),
    ("role", "role"),
    ("defaultValue", "value"),
    ("accept", "accept"),
    ("autocomplete", "autocomplete"),
    ("pattern", "pattern"),
    ("min", "min"),
    ("max", "max"),
    ("step", "step"),
    ("label", "label"),
    ("content", "content"),
    ("httpEquiv", "http-equiv"),
    ("charset", "charset"),
    ("media", "media"),
    ("enctype", "enctype"),
    ("download", "download"),
    ("hreflang", "hreflang"),
    ("srcset", "srcset"),
    ("sizes", "sizes"),
    ("loading", "loading"),
    ("decoding", "decoding"),
    ("crossOrigin", "crossorigin"),
    ("integrity", "integrity"),
    ("referrerPolicy", "referrerpolicy"),
    ("contentEditable", "contenteditable"),
    ("slot", "slot"),
    ("accessKey", "accesskey"),
    ("inputMode", "inputmode"),
    ("enterKeyHint", "enterkeyhint"),
    ("nonce", "nonce"),
    ("wrap", "wrap"),
    ("scope", "scope"),
    ("headers", "headers"),
    ("abbr", "abbr"),
    ("shape", "shape"),
    ("coords", "coords"),
    ("useMap", "usemap"),
    ("align", "align"),
    ("valign", "valign"),
    ("bgColor", "bgcolor"),
    ("color", "color"),
    ("face", "face"),
    ("size", "size"),
    ("width", "width"),
    ("height", "height"),
];

fn node_get(it: &mut Interp, n: NodeId, key: &str) -> R<Option<Value>> {
    let is_doc = n == Document::ROOT;
    let is_element = it.host.doc.element(n).is_some();
    // Properties every node has.
    let v = match key {
        "nodeType" => Value::Num(node_type(&it.host.doc, n)),
        "nodeName" => s(&match &it.host.doc.nodes[n].data {
            NodeData::Element(e) if e.name == FRAGMENT => "#document-fragment".to_string(),
            NodeData::Element(e) => e.name.to_ascii_uppercase(),
            NodeData::Text(_) => "#text".into(),
            NodeData::Comment(_) => "#comment".into(),
            NodeData::Document => "#document".into(),
            NodeData::Doctype(d) => d.clone(),
        }),
        "nodeValue" | "data" if !is_element && !is_doc => match &it.host.doc.nodes[n].data {
            NodeData::Text(t) | NodeData::Comment(t) => s(t),
            _ => Value::Null,
        },
        "nodeValue" => Value::Null,
        "length" if matches!(it.host.doc.nodes[n].data, NodeData::Text(_) | NodeData::Comment(_)) => match &it.host.doc.nodes[n].data {
            NodeData::Text(t) | NodeData::Comment(t) => Value::Num(t.encode_utf16().count() as f64),
            _ => Value::Num(0.0),
        },
        "wholeText" => s(&it.host.doc.text_content(n)),
        "textContent" => {
            if is_doc {
                Value::Null
            } else {
                s(&it.host.doc.text_content(n))
            }
        }
        "parentNode" => {
            let p = it.host.doc.parent(n);
            wrap_opt(it, p)
        }
        "parentElement" => {
            let p = it.host.doc.parent(n).filter(|&p| it.host.doc.element(p).is_some());
            wrap_opt(it, p)
        }
        "childNodes" => {
            let k = it.host.doc.children(n).to_vec();
            node_list(it, k)
        }
        "children" => {
            let k = element_children(it, n);
            node_list(it, k)
        }
        "childElementCount" => Value::Num(element_children(it, n).len() as f64),
        "firstChild" => {
            let c = it.host.doc.children(n).first().copied();
            wrap_opt(it, c)
        }
        "lastChild" => {
            let c = it.host.doc.children(n).last().copied();
            wrap_opt(it, c)
        }
        "firstElementChild" => {
            let c = element_children(it, n).first().copied();
            wrap_opt(it, c)
        }
        "lastElementChild" => {
            let c = element_children(it, n).last().copied();
            wrap_opt(it, c)
        }
        "nextSibling" => {
            let c = sibling(it, n, 1, false);
            wrap_opt(it, c)
        }
        "previousSibling" => {
            let c = sibling(it, n, -1, false);
            wrap_opt(it, c)
        }
        "nextElementSibling" => {
            let c = sibling(it, n, 1, true);
            wrap_opt(it, c)
        }
        "previousElementSibling" => {
            let c = sibling(it, n, -1, true);
            wrap_opt(it, c)
        }
        "ownerDocument" => {
            if is_doc {
                Value::Null
            } else {
                wrap(it, Document::ROOT)
            }
        }
        "isConnected" => Value::Bool(connected(&it.host.doc, n)),
        _ => {
            if is_doc {
                return document_get(it, key);
            }
            if !is_element {
                return Ok(None);
            }
            return element_get(it, n, key);
        }
    };
    Ok(Some(v))
}

fn document_get(it: &mut Interp, key: &str) -> R<Option<Value>> {
    let v = match key {
        "documentElement" => {
            let h = it.host.doc.html_element();
            wrap_opt(it, h)
        }
        "body" => {
            let b = it.host.doc.body();
            wrap_opt(it, b)
        }
        "head" => {
            let h = it.host.doc.html_element().and_then(|h| it.host.doc.child_elements(h).find(|&c| it.host.doc.is(c, "head")));
            wrap_opt(it, h)
        }
        "title" => s(&it.host.doc.title()),
        "readyState" => s(it.host.ready),
        "URL" | "documentURI" | "baseURI" => s(&it.host.url.as_ref().map(|u| u.as_str().to_string()).unwrap_or_else(|| "about:blank".into())),
        "domain" => s(it.host.url.as_ref().map(|u| u.host()).unwrap_or("")),
        "referrer" => s(""),
        "cookie" => {
            let o = it.host.origin();
            let st = it.host.storage.borrow();
            let c = st.cookies.get(&o).map(|v| v.iter().map(|(k, v)| if k.is_empty() { v.clone() } else { format!("{k}={v}") }).collect::<Vec<_>>().join("; ")).unwrap_or_default();
            s(&c)
        }
        "characterSet" | "charset" | "inputEncoding" => s("UTF-8"),
        "contentType" => s("text/html"),
        "compatMode" => s(if it.host.doc.quirks { "BackCompat" } else { "CSS1Compat" }),
        "visibilityState" => s("visible"),
        "hidden" => Value::Bool(false),
        "defaultView" => Value::Obj(it.global),
        "location" => it.get(&Value::Obj(it.global), "location")?,
        "activeElement" => {
            let f = it.host.focused.or_else(|| it.host.doc.body());
            wrap_opt(it, f)
        }
        "currentScript" => {
            let c = it.host.current_script;
            wrap_opt(it, c)
        }
        "scrollingElement" => {
            let h = it.host.doc.html_element();
            wrap_opt(it, h)
        }
        "forms" => {
            let f = elements_where(it, Document::ROOT, |e| e.name == "form")?;
            node_list(it, f)
        }
        "links" => {
            let f = elements_where(it, Document::ROOT, |e| (e.name == "a" || e.name == "area") && e.attr("href").is_some())?;
            node_list(it, f)
        }
        "images" => {
            let f = elements_where(it, Document::ROOT, |e| e.name == "img")?;
            node_list(it, f)
        }
        "scripts" => {
            let f = elements_where(it, Document::ROOT, |e| e.name == "script")?;
            node_list(it, f)
        }
        "styleSheets" => it.new_array(Vec::new()),
        "fonts" => {
            let o = it.new_object();
            let p = super::builtins::promise_resolved(it, Value::Obj(o))?;
            it.set_prop(o, "ready", p);
            it.set_prop(o, "status", s("loaded"));
            Value::Obj(o)
        }
        "doctype" => {
            let d = it.host.doc.children(Document::ROOT).iter().copied().find(|&c| matches!(it.host.doc.nodes[c].data, NodeData::Doctype(_)));
            wrap_opt(it, d)
        }
        "implementation" => {
            let o = it.new_object();
            it.method(o, "hasFeature", |_, _| Ok(Value::Bool(true)), 0);
            Value::Obj(o)
        }
        "designMode" => s("off"),
        "fullscreenElement" | "pointerLockElement" => Value::Null,
        "fullscreenEnabled" => Value::Bool(false),
        _ => return Ok(None),
    };
    Ok(Some(v))
}

fn element_get(it: &mut Interp, n: NodeId, key: &str) -> R<Option<Value>> {
    if let Some((_, a)) = STR_ATTRS.iter().find(|(k, _)| *k == key) {
        return Ok(Some(s(&attr(it, n, a).unwrap_or_default())));
    }
    if let Some((_, a)) = BOOL_ATTRS.iter().find(|(k, _)| *k == key) {
        return Ok(Some(Value::Bool(attr(it, n, a).is_some())));
    }
    let name = it.host.doc.name(n).to_string();
    let v = match key {
        "tagName" => s(&name.to_ascii_uppercase()),
        "localName" => s(&name),
        "namespaceURI" => s("http://www.w3.org/1999/xhtml"),
        "prefix" => Value::Null,
        "innerHTML" => s(&serialize(&it.host.doc, n, true)),
        "outerHTML" => s(&serialize(&it.host.doc, n, false)),
        "innerText" | "outerText" => s(&it.host.doc.text_content(n)),
        "classList" | "relList" => sub_object(it, n, "classList"),
        "style" => sub_object(it, n, "style"),
        "dataset" => sub_object(it, n, "dataset"),
        "attributes" => {
            let attrs = el(it, n).map(|e| e.attrs.clone()).unwrap_or_default();
            let mut v = Vec::new();
            for (k, val) in attrs {
                let o = it.new_object();
                it.set_prop(o, "name", s(&k));
                it.set_prop(o, "localName", s(&k));
                it.set_prop(o, "value", s(&val));
                it.set_prop(o, "nodeValue", s(&val));
                v.push(Value::Obj(o));
            }
            it.new_array(v)
        }
        "href" if matches!(name.as_str(), "a" | "area" | "link" | "base") => s(&resolved_attr(it, n, "href")),
        "src" if matches!(name.as_str(), "script" | "img" | "iframe" | "input" | "source" | "video" | "audio" | "embed" | "track") => s(&resolved_attr(it, n, "src")),
        "action" if name == "form" => {
            let a = resolved_attr(it, n, "action");
            s(&if a.is_empty() { it.host.url.as_ref().map(|u| u.as_str().to_string()).unwrap_or_default() } else { a })
        }
        "protocol" | "host" | "hostname" | "port" | "pathname" | "search" | "hash" | "origin" if name == "a" || name == "area" => {
            let h = resolved_attr(it, n, "href");
            match Url::parse(&h) {
                Ok(u) => s(&super::window::url_part(&u, key)),
                Err(_) => s(""),
            }
        }
        "type" => s(&match name.as_str() {
            "input" => input_type(it, n),
            "button" => attr(it, n, "type").map(|t| t.to_ascii_lowercase()).unwrap_or_else(|| "submit".into()),
            "select" => if attr(it, n, "multiple").is_some() { "select-multiple".into() } else { "select-one".into() },
            "textarea" => "textarea".into(),
            _ => attr(it, n, "type").unwrap_or_default(),
        }),
        "value" if matches!(name.as_str(), "input" | "textarea" | "select" | "option" | "button" | "data" | "param" | "li" | "output") => s(&get_value(it, n)),
        "valueAsNumber" if name == "input" => Value::Num(super::value::str_to_num(&get_value(it, n))),
        "checked" if name == "input" => Value::Bool(it.host.form.checked.get(&n).copied().unwrap_or(false)),
        "indeterminate" => Value::Bool(false),
        "selected" if name == "option" => Value::Bool(attr(it, n, "selected").is_some() || it.host.doc.parent(n).and_then(|p| { let sel = if is_el(it, p, "select") { p } else { it.host.doc.parent(p)? }; selected_index(it, sel).and_then(|i| options_of(it, sel).get(i).copied()) }) == Some(n)),
        "text" if name == "option" || name == "a" || name == "title" || name == "script" => s(&it.host.doc.text_content(n)),
        "index" if name == "option" => {
            let sel = it.host.doc.parent(n);
            Value::Num(sel.map(|p| options_of(it, p).iter().position(|&o| o == n).unwrap_or(0)).unwrap_or(0) as f64)
        }
        "selectedIndex" if name == "select" => Value::Num(selected_index(it, n).map(|i| i as f64).unwrap_or(-1.0)),
        "options" if name == "select" => {
            let o = options_of(it, n);
            node_list(it, o)
        }
        "selectedOptions" if name == "select" => {
            let o: Vec<NodeId> = selected_index(it, n).and_then(|i| options_of(it, n).get(i).copied()).into_iter().collect();
            node_list(it, o)
        }
        "length" if name == "select" => Value::Num(options_of(it, n).len() as f64),
        "length" if name == "form" => Value::Num(form_controls(it, n).len() as f64),
        "elements" if name == "form" || name == "fieldset" => {
            let c = form_controls(it, n);
            node_list(it, c)
        }
        "form" if matches!(name.as_str(), "input" | "select" | "textarea" | "button" | "fieldset" | "output" | "label" | "option") => {
            let f = form_of(it, n);
            wrap_opt(it, f)
        }
        "labels" => it.new_array(Vec::new()),
        "maxLength" | "minLength" => Value::Num(attr(it, n, &key.to_ascii_lowercase()).and_then(|v| v.trim().parse::<f64>().ok()).unwrap_or(-1.0)),
        "tabIndex" => Value::Num(attr(it, n, "tabindex").and_then(|v| v.trim().parse::<f64>().ok()).unwrap_or(if matches!(name.as_str(), "a" | "input" | "select" | "textarea" | "button") { 0.0 } else { -1.0 })),
        "colSpan" | "rowSpan" => Value::Num(attr(it, n, &key.to_ascii_lowercase()).and_then(|v| v.trim().parse::<f64>().ok()).unwrap_or(1.0)),
        "rows" if name == "table" => {
            let r = elements_where(it, n, |e| e.name == "tr")?;
            node_list(it, r)
        }
        "cells" if name == "tr" => {
            let c: Vec<NodeId> = element_children(it, n).into_iter().filter(|&c| matches!(it.host.doc.name(c), "td" | "th")).collect();
            node_list(it, c)
        }
        "tBodies" if name == "table" => {
            let c: Vec<NodeId> = element_children(it, n).into_iter().filter(|&c| it.host.doc.is(c, "tbody")).collect();
            node_list(it, c)
        }
        "content" if name == "template" => {
            // Template contents live as its children here.
            wrap(it, n)
        }
        "sheet" => Value::Null,
        "naturalWidth" | "naturalHeight" => Value::Num(0.0),
        "complete" => Value::Bool(true),
        "currentSrc" => s(&resolved_attr(it, n, "src")),
        "offsetWidth" | "clientWidth" | "scrollWidth" => {
            if Some(n) == it.host.doc.html_element() && key != "offsetWidth" {
                Value::Num(it.host.viewport.0 as f64)
            } else {
                Value::Num(rect_of(it, n)[2].round() as f64)
            }
        }
        "offsetHeight" | "clientHeight" | "scrollHeight" => {
            let html = it.host.doc.html_element();
            if Some(n) == html && key == "clientHeight" {
                Value::Num(it.host.viewport.1 as f64)
            } else if (Some(n) == html || Some(n) == it.host.doc.body()) && key == "scrollHeight" {
                Value::Num(it.host.page_h.max(it.host.viewport.1) as f64)
            } else {
                Value::Num(rect_of(it, n)[3].round() as f64)
            }
        }
        "offsetTop" => Value::Num(rect_of(it, n)[1].round() as f64),
        "offsetLeft" => Value::Num(rect_of(it, n)[0].round() as f64),
        "clientTop" | "clientLeft" | "scrollLeft" => Value::Num(0.0),
        "scrollTop" => {
            let html = it.host.doc.html_element();
            Value::Num(if Some(n) == html || Some(n) == it.host.doc.body() { it.host.scroll.1 as f64 } else { 0.0 })
        }
        "offsetParent" => {
            let b = it.host.doc.body();
            wrap_opt(it, b)
        }
        "shadowRoot" | "assignedSlot" => Value::Null,
        "validity" => {
            let o = it.new_object();
            it.set_prop(o, "valid", Value::Bool(true));
            Value::Obj(o)
        }
        "validationMessage" => s(""),
        "willValidate" => Value::Bool(false),
        "isContentEditable" => Value::Bool(false),
        "draggable" | "spellcheck" | "translate" => Value::Bool(false),
        "files" => Value::Null,
        _ => return Ok(None),
    };
    Ok(Some(v))
}

fn node_set(it: &mut Interp, n: NodeId, key: &str, v: &Value) -> R<bool> {
    let is_element = it.host.doc.element(n).is_some();
    if n == Document::ROOT {
        match key {
            "title" => {
                let t = it.to_str(v)?.to_string();
                let title = match it.host.doc.find("title") {
                    Some(t) => t,
                    None => {
                        let te = new_element(it, "title")?;
                        let head = it.host.doc.html_element().and_then(|h| it.host.doc.child_elements(h).find(|&c| it.host.doc.is(c, "head")));
                        if let Some(h) = head {
                            it.host.doc.append(h, te);
                        }
                        te
                    }
                };
                set_text(it, title, &t)?;
                return Ok(true);
            }
            "cookie" => {
                let t = it.to_str(v)?.to_string();
                set_cookie(it, &t);
                return Ok(true);
            }
            "location" => {
                let t = it.to_str(v)?.to_string();
                super::window::navigate(it, &t, false);
                return Ok(true);
            }
            "body" | "designMode" | "dir" | "domain" => return Ok(true),
            _ => return Ok(false),
        }
    }
    match key {
        "textContent" | "nodeValue" | "data" | "innerText" | "outerText" | "text" if !(key == "text" && !matches!(it.host.doc.name(n), "option" | "a" | "title" | "script")) => {
            if (key == "nodeValue" || key == "data") && is_element {
                return Ok(true);
            }
            let t = if v.is_nullish() { String::new() } else { it.to_str(v)?.to_string() };
            if key == "outerText" {
                let tn = new_node(it, NodeData::Text(t))?;
                if let Some(p) = it.host.doc.parent(n) {
                    it.host.doc.insert_before(p, tn, n);
                    remove_node(it, n);
                }
                return Ok(true);
            }
            set_text(it, n, &t)?;
            return Ok(true);
        }
        _ => {}
    }
    if !is_element {
        return Ok(false);
    }
    if let Some((_, a)) = STR_ATTRS.iter().find(|(k, _)| *k == key) {
        let t = it.to_str(v)?.to_string();
        set_attr(it, n, a, &t);
        return Ok(true);
    }
    if let Some((_, a)) = BOOL_ATTRS.iter().find(|(k, _)| *k == key) {
        if Interp::truthy(v) {
            set_attr(it, n, a, "");
        } else {
            remove_attr(it, n, a);
        }
        return Ok(true);
    }
    let name = it.host.doc.name(n).to_string();
    match key {
        "innerHTML" => {
            let t = if v.is_nullish() { String::new() } else { it.to_str(v)?.to_string() };
            set_inner_html(it, n, &t)?;
        }
        "outerHTML" => {
            let t = it.to_str(v)?.to_string();
            if let Some(p) = it.host.doc.parent(n) {
                let ctx = it.host.doc.name(p).to_string();
                let nodes = parse_fragment(it, &ctx, &t)?;
                for c in nodes {
                    it.host.doc.insert_before(p, c, n);
                }
                remove_node(it, n);
            }
        }
        "value" if matches!(name.as_str(), "input" | "textarea" | "select" | "option" | "button" | "data" | "output") => {
            let t = if v.is_nullish() { String::new() } else { it.to_str(v)?.to_string() };
            set_value(it, n, &t);
        }
        "checked" if name == "input" => {
            let on = Interp::truthy(v);
            set_checked(it, n, on);
        }
        "selected" if name == "option" => {
            if let Some(sel) = it.host.doc.parent(n).and_then(|p| if is_el(it, p, "select") { Some(p) } else { it.host.doc.parent(p) }) {
                if Interp::truthy(v) {
                    let i = options_of(it, sel).iter().position(|&o| o == n);
                    select_index(it, sel, i);
                } else {
                    remove_attr(it, n, "selected");
                }
            }
        }
        "selectedIndex" if name == "select" => {
            let i = it.to_num(v)?;
            select_index(it, n, if i >= 0.0 { Some(i as usize) } else { None });
        }
        "href" | "src" | "action" | "type" | "tabIndex" | "maxLength" | "minLength" | "colSpan" | "rowSpan" => {
            let t = it.to_str(v)?.to_string();
            let a = match key {
                "tabIndex" => "tabindex".to_string(),
                "maxLength" | "minLength" | "colSpan" | "rowSpan" => key.to_ascii_lowercase(),
                k => k.to_string(),
            };
            set_attr(it, n, &a, &t);
            if key == "src" && name == "script" && connected(&it.host.doc, n) && !it.host.started.contains(&n) {
                start_script(it, n)?;
            }
        }
        "scrollTop" => {
            let html = it.host.doc.html_element();
            if Some(n) == html || Some(n) == it.host.doc.body() {
                let y = it.to_num(v)? as f32;
                it.host.scroll_req = Some((0.0, y));
            }
        }
        "scrollLeft" | "indeterminate" | "spellcheck" | "draggable" | "translate" | "valueAsNumber" => {}
        "classList" => {
            let t = it.to_str(v)?.to_string();
            set_attr(it, n, "class", &t);
        }
        "style" => {
            let t = it.to_str(v)?.to_string();
            set_attr(it, n, "style", &t);
        }
        _ => return Ok(false),
    }
    Ok(true)
}

fn set_cookie(it: &mut Interp, t: &str) {
    let mut parts = t.split(';');
    let first = parts.next().unwrap_or("").trim();
    let (k, v) = match first.split_once('=') {
        Some((k, v)) => (k.trim().to_string(), v.trim().to_string()),
        None => (String::new(), first.to_string()),
    };
    let mut delete = false;
    for p in parts {
        let p = p.trim().to_ascii_lowercase();
        if p == "max-age=0" || p.starts_with("max-age=-") {
            delete = true;
        }
        if let Some(d) = p.strip_prefix("expires=") {
            let ms = super::builtins::parse_date(d);
            if !ms.is_nan() && ms < super::builtins::now_ms() {
                delete = true;
            }
        }
    }
    let o = it.host.origin();
    let mut st = it.host.storage.borrow_mut();
    let jar = st.cookies.entry(o).or_default();
    jar.retain(|(x, _)| *x != k);
    if !delete && jar.len() < 200 && k.len() + v.len() < 8192 {
        jar.push((k, v));
    }
}

// ---- methods on nodes -----------------------------------------------------------------

fn install_node(it: &mut Interp) {
    let proto = it.new_object();
    it.p.node = Some(proto);
    install_event_target(it, proto);
    let p = proto;
    it.method(p, "getAttribute", |it, a| {
        let n = this_node(it, a)?;
        let k = str_arg(it, a, 0)?.to_ascii_lowercase();
        Ok(attr(it, n, &k).map(|v| s(&v)).unwrap_or(Value::Null))
    }, 1);
    it.method(p, "setAttribute", |it, a| {
        let n = this_node(it, a)?;
        let k = str_arg(it, a, 0)?.to_ascii_lowercase();
        if k.is_empty() || k.chars().any(|c| c.is_whitespace() || "\"'>/=".contains(c)) {
            return Err(it.err(ErrKind::Error, "InvalidCharacterError: not a valid attribute name"));
        }
        let v = str_arg(it, a, 1)?;
        set_attr(it, n, &k, &v);
        if k == "src" && is_el(it, n, "script") && connected(&it.host.doc, n) && !it.host.started.contains(&n) {
            start_script(it, n)?;
        }
        if k == "value" && is_el(it, n, "input") && !it.host.form.values.contains_key(&n) {
            it.host.repaint = true;
        }
        Ok(Value::Undef)
    }, 2);
    it.method(p, "removeAttribute", |it, a| {
        let n = this_node(it, a)?;
        let k = str_arg(it, a, 0)?.to_ascii_lowercase();
        remove_attr(it, n, &k);
        Ok(Value::Undef)
    }, 1);
    it.method(p, "hasAttribute", |it, a| {
        let n = this_node(it, a)?;
        let k = str_arg(it, a, 0)?.to_ascii_lowercase();
        Ok(Value::Bool(attr(it, n, &k).is_some()))
    }, 1);
    it.method(p, "hasAttributes", |it, a| {
        let n = this_node(it, a)?;
        Ok(Value::Bool(el(it, n).is_some_and(|e| !e.attrs.is_empty())))
    }, 0);
    it.method(p, "toggleAttribute", |it, a| {
        let n = this_node(it, a)?;
        let k = str_arg(it, a, 0)?.to_ascii_lowercase();
        let has = attr(it, n, &k).is_some();
        let want = if a.args.len() > 1 { Interp::truthy(&a.get(1)) } else { !has };
        if want && !has {
            set_attr(it, n, &k, "");
        } else if !want && has {
            remove_attr(it, n, &k);
        }
        Ok(Value::Bool(want))
    }, 1);
    it.method(p, "getAttributeNames", |it, a| {
        let n = this_node(it, a)?;
        let names: Vec<Value> = el(it, n).map(|e| e.attrs.iter().map(|(k, _)| s(k)).collect()).unwrap_or_default();
        Ok(it.new_array(names))
    }, 0);
    it.method(p, "getAttributeNS", |it, a| {
        let n = this_node(it, a)?;
        let k = str_arg(it, a, 1)?.to_ascii_lowercase();
        Ok(attr(it, n, &k).map(|v| s(&v)).unwrap_or(Value::Null))
    }, 2);
    it.method(p, "setAttributeNS", |it, a| {
        let n = this_node(it, a)?;
        let k = str_arg(it, a, 1)?.to_ascii_lowercase();
        let v = str_arg(it, a, 2)?;
        set_attr(it, n, &k, &v);
        Ok(Value::Undef)
    }, 3);
    it.method(p, "appendChild", |it, a| {
        let n = this_node(it, a)?;
        let c = arg_node(it, &a.get(0))?;
        insert(it, n, c, None)?;
        Ok(a.get(0))
    }, 1);
    it.method(p, "insertBefore", |it, a| {
        let n = this_node(it, a)?;
        let c = arg_node(it, &a.get(0))?;
        let before = if a.get(1).is_nullish() { None } else { Some(arg_node(it, &a.get(1))?) };
        insert(it, n, c, before)?;
        Ok(a.get(0))
    }, 2);
    it.method(p, "removeChild", |it, a| {
        let n = this_node(it, a)?;
        let c = arg_node(it, &a.get(0))?;
        if it.host.doc.parent(c) != Some(n) {
            return Err(it.err(ErrKind::Error, "NotFoundError: the node to be removed is not a child of this node"));
        }
        remove_node(it, c);
        Ok(a.get(0))
    }, 1);
    it.method(p, "replaceChild", |it, a| {
        let n = this_node(it, a)?;
        let new = arg_node(it, &a.get(0))?;
        let old = arg_node(it, &a.get(1))?;
        if it.host.doc.parent(old) != Some(n) {
            return Err(it.err(ErrKind::Error, "NotFoundError: the node to be replaced is not a child of this node"));
        }
        if new != old {
            insert(it, n, new, Some(old))?;
            remove_node(it, old);
        }
        Ok(a.get(1))
    }, 2);
    it.method(p, "remove", |it, a| {
        let n = this_node(it, a)?;
        remove_node(it, n);
        Ok(Value::Undef)
    }, 0);
    fn place(it: &mut Interp, a: &A, mode: u8) -> R<Value> {
        let n = this_node(it, a)?;
        let nodes = nodes_from_args(it, a)?;
        match mode {
            // append / prepend
            0 | 1 => {
                let first = if mode == 1 { it.host.doc.children(n).first().copied() } else { None };
                for c in nodes {
                    insert(it, n, c, first)?;
                }
            }
            // before / after / replaceWith
            _ => {
                let Some(p) = it.host.doc.parent(n) else { return Ok(Value::Undef) };
                let anchor = match mode {
                    2 => Some(n),
                    _ => sibling(it, n, 1, false),
                };
                for c in nodes {
                    if c == n {
                        continue;
                    }
                    insert(it, p, c, anchor)?;
                }
                if mode == 4 {
                    remove_node(it, n);
                }
            }
        }
        Ok(Value::Undef)
    }
    it.method(p, "append", |it, a| place(it, a, 0), 0);
    it.method(p, "prepend", |it, a| place(it, a, 1), 0);
    it.method(p, "before", |it, a| place(it, a, 2), 0);
    it.method(p, "after", |it, a| place(it, a, 3), 0);
    it.method(p, "replaceWith", |it, a| place(it, a, 4), 0);
    it.method(p, "replaceChildren", |it, a| {
        let n = this_node(it, a)?;
        let nodes = nodes_from_args(it, a)?;
        remove_children(it, n);
        for c in nodes {
            insert(it, n, c, None)?;
        }
        Ok(Value::Undef)
    }, 0);
    it.method(p, "cloneNode", |it, a| {
        let n = this_node(it, a)?;
        let c = clone_node(it, n, Interp::truthy(&a.get(0)))?;
        Ok(wrap(it, c))
    }, 1);
    it.method(p, "importNode", |it, a| {
        let n = arg_node(it, &a.get(0))?;
        let c = clone_node(it, n, Interp::truthy(&a.get(1)))?;
        Ok(wrap(it, c))
    }, 2);
    it.method(p, "adoptNode", |it, a| {
        let n = arg_node(it, &a.get(0))?;
        remove_node(it, n);
        Ok(a.get(0))
    }, 1);
    it.method(p, "contains", |it, a| {
        let n = this_node(it, a)?;
        let Some(c) = node_of(it, &a.get(0)) else { return Ok(Value::Bool(false)) };
        Ok(Value::Bool(contains(&it.host.doc, n, c)))
    }, 1);
    it.method(p, "hasChildNodes", |it, a| {
        let n = this_node(it, a)?;
        Ok(Value::Bool(!it.host.doc.children(n).is_empty()))
    }, 0);
    it.method(p, "getRootNode", |it, a| {
        let n = this_node(it, a)?;
        let mut cur = n;
        while let Some(p) = it.host.doc.parent(cur) {
            cur = p;
        }
        Ok(wrap(it, cur))
    }, 0);
    it.method(p, "isEqualNode", |it, a| {
        let n = this_node(it, a)?;
        let Some(o) = node_of(it, &a.get(0)) else { return Ok(Value::Bool(false)) };
        Ok(Value::Bool(serialize(&it.host.doc, n, false) == serialize(&it.host.doc, o, false)))
    }, 1);
    it.method(p, "isSameNode", |it, a| {
        let n = this_node(it, a)?;
        Ok(Value::Bool(node_of(it, &a.get(0)) == Some(n)))
    }, 1);
    it.method(p, "compareDocumentPosition", |it, a| {
        let n = this_node(it, a)?;
        let o = arg_node(it, &a.get(0))?;
        if n == o {
            return Ok(Value::Num(0.0));
        }
        if contains(&it.host.doc, n, o) {
            return Ok(Value::Num(20.0));
        }
        if contains(&it.host.doc, o, n) {
            return Ok(Value::Num(10.0));
        }
        let order = it.host.doc.descendants(Document::ROOT);
        let (pn, po) = (order.iter().position(|&x| x == n), order.iter().position(|&x| x == o));
        Ok(Value::Num(match (pn, po) {
            (Some(x), Some(y)) if x < y => 4.0,
            (Some(_), Some(_)) => 2.0,
            _ => 1.0 + 32.0,
        }))
    }, 1);
    it.method(p, "normalize", |_, _| Ok(Value::Undef), 0);
    it.method(p, "querySelector", |it, a| {
        let n = this_node(it, a)?;
        let sel = str_arg(it, a, 0)?;
        let r = query(it, n, &sel, false)?;
        Ok(wrap_opt(it, r.first().copied()))
    }, 1);
    it.method(p, "querySelectorAll", |it, a| {
        let n = this_node(it, a)?;
        let sel = str_arg(it, a, 0)?;
        let r = query(it, n, &sel, true)?;
        Ok(node_list(it, r))
    }, 1);
    it.method(p, "matches", |it, a| {
        let n = this_node(it, a)?;
        let sel = str_arg(it, a, 0)?;
        let sels = selectors(it, &sel)?;
        Ok(Value::Bool(matches_any(&it.host.doc, n, &sels)))
    }, 1);
    it.method(p, "closest", |it, a| {
        let n = this_node(it, a)?;
        let sel = str_arg(it, a, 0)?;
        let sels = selectors(it, &sel)?;
        let mut cur = Some(n);
        while let Some(c) = cur {
            if matches_any(&it.host.doc, c, &sels) {
                return Ok(wrap(it, c));
            }
            cur = it.host.doc.parent(c);
        }
        Ok(Value::Null)
    }, 1);
    it.method(p, "getElementsByTagName", |it, a| {
        let n = this_node(it, a)?;
        let t = str_arg(it, a, 0)?.to_ascii_lowercase();
        let r = elements_where(it, n, |e| t == "*" || e.name == t)?;
        Ok(node_list(it, r))
    }, 1);
    it.method(p, "getElementsByClassName", |it, a| {
        let n = this_node(it, a)?;
        let t = str_arg(it, a, 0)?;
        let want: Vec<String> = t.split_ascii_whitespace().map(|s| s.to_string()).collect();
        let r = elements_where(it, n, |e| !want.is_empty() && want.iter().all(|c| e.has_class(c)))?;
        Ok(node_list(it, r))
    }, 1);
    it.method(p, "getElementsByName", |it, a| {
        let t = str_arg(it, a, 0)?;
        let r = elements_where(it, Document::ROOT, |e| e.attr("name") == Some(t.as_str()))?;
        Ok(node_list(it, r))
    }, 1);
    it.method(p, "getElementById", |it, a| {
        let t = str_arg(it, a, 0)?;
        let r = get_element_by_id(&it.host.doc, &t);
        it.steps += (it.host.doc.nodes.len() / 8) as u64;
        Ok(wrap_opt(it, r))
    }, 1);
    it.method(p, "createElement", |it, a| {
        let t = str_arg(it, a, 0)?;
        let n = new_element(it, &t)?;
        Ok(wrap(it, n))
    }, 1);
    it.method(p, "createElementNS", |it, a| {
        let t = str_arg(it, a, 1)?;
        let n = new_element(it, &t)?;
        let ns = str_arg(it, a, 0)?;
        if ns.contains("svg") || ns.contains("MathML") {
            if let Some(e) = it.host.doc.element_mut(n) {
                e.foreign = true;
            }
        }
        Ok(wrap(it, n))
    }, 2);
    it.method(p, "createTextNode", |it, a| {
        let t = str_arg(it, a, 0)?;
        let n = new_node(it, NodeData::Text(t))?;
        Ok(wrap(it, n))
    }, 1);
    it.method(p, "createComment", |it, a| {
        let t = str_arg(it, a, 0)?;
        let n = new_node(it, NodeData::Comment(t))?;
        Ok(wrap(it, n))
    }, 1);
    it.method(p, "createDocumentFragment", |it, _| {
        let n = new_node(it, NodeData::Element(Element { name: FRAGMENT.into(), attrs: Vec::new(), foreign: false }))?;
        Ok(wrap(it, n))
    }, 0);
    it.method(p, "createEvent", |it, _| {
        let g = new_event(it, "", &EventInit::default());
        it.set_prop(g, "isTrusted", Value::Bool(false));
        Ok(Value::Obj(g))
    }, 1);
    it.method(p, "createRange", |it, _| {
        let o = it.new_object();
        it.method(o, "selectNodeContents", |_, _| Ok(Value::Undef), 1);
        it.method(o, "setStart", |_, _| Ok(Value::Undef), 2);
        it.method(o, "setEnd", |_, _| Ok(Value::Undef), 2);
        it.method(o, "collapse", |_, _| Ok(Value::Undef), 1);
        it.method(o, "createContextualFragment", |it, a| {
            let t = str_arg(it, a, 0)?;
            let f = new_node(it, NodeData::Element(Element { name: FRAGMENT.into(), attrs: Vec::new(), foreign: false }))?;
            let nodes = parse_fragment(it, "div", &t)?;
            for c in nodes {
                it.host.doc.append(f, c);
            }
            // Scripts in a contextual fragment do run when inserted.
            for d in it.host.doc.descendants(f) {
                it.host.started.remove(&d);
            }
            Ok(wrap(it, f))
        }, 1);
        it.method(o, "getBoundingClientRect", |it, _| Ok(rect_object(it, [0.0; 4])), 0);
        Ok(Value::Obj(o))
    }, 0);
    it.method(p, "getSelection", |_, _| Ok(Value::Null), 0);
    it.method(p, "hasFocus", |_, _| Ok(Value::Bool(true)), 0);
    it.method(p, "elementFromPoint", |it, a| {
        let x = it.to_num(&a.get(0))? as f32 + it.host.scroll.0;
        let y = it.to_num(&a.get(1))? as f32 + it.host.scroll.1;
        let mut best: Option<(NodeId, f32)> = None;
        for (&n, r) in &it.host.rects {
            if x >= r[0] && x < r[0] + r[2] && y >= r[1] && y < r[1] + r[3] {
                let area = r[2] * r[3];
                if best.is_none_or(|(_, a)| area < a) {
                    best = Some((n, area));
                }
            }
        }
        Ok(wrap_opt(it, best.map(|b| b.0)))
    }, 2);
    fn write(it: &mut Interp, a: &A, nl: bool) -> R<Value> {
        let mut t = String::new();
        for v in a.args {
            t.push_str(&it.to_str(v)?);
        }
        if nl {
            t.push('\n');
        }
        let Some(point) = it.host.write_point else {
            it.host.log("document.write after the page loaded was ignored".into());
            return Ok(Value::Undef);
        };
        let Some(p) = it.host.doc.parent(point) else { return Ok(Value::Undef) };
        let ctx = it.host.doc.name(p).to_string();
        let nodes = parse_fragment(it, &ctx, &t)?;
        let mut after = point;
        for c in nodes {
            let next = sibling(it, after, 1, false);
            match next {
                Some(nx) => it.host.doc.insert_before(p, c, nx),
                None => it.host.doc.append(p, c),
            }
            after = c;
            // Written scripts run (they were marked started by import).
            for d in it.host.doc.descendants(c) {
                if it.host.doc.is(d, "script") {
                    it.host.started.remove(&d);
                }
            }
            start_scripts_in(it, c)?;
        }
        it.host.write_point = Some(after);
        it.host.dirty = true;
        Ok(Value::Undef)
    }
    it.method(p, "write", |it, a| write(it, a, false), 1);
    it.method(p, "writeln", |it, a| write(it, a, true), 1);
    it.method(p, "open", |it, a| Ok(a.this.clone()).map(|v| { let _ = it; v }), 0);
    it.method(p, "close", |_, _| Ok(Value::Undef), 0);
    it.method(p, "execCommand", |_, _| Ok(Value::Bool(false)), 1);
    it.method(p, "queryCommandSupported", |_, _| Ok(Value::Bool(false)), 1);
    it.method(p, "exitFullscreen", |it, _| super::builtins::promise_resolved(it, Value::Undef), 0);
    it.method(p, "requestFullscreen", |it, _| {
        let e = it.make_error(ErrKind::Type, "fullscreen isn't available");
        Ok(super::builtins::promise_rejected(it, e))
    }, 0);
    it.method(p, "getBoundingClientRect", |it, a| {
        let n = this_node(it, a)?;
        let r = rect_of(it, n);
        Ok(rect_object(it, r))
    }, 0);
    it.method(p, "getClientRects", |it, a| {
        let n = this_node(it, a)?;
        let r = rect_of(it, n);
        let o = rect_object(it, r);
        Ok(it.new_array(if r[2] > 0.0 || r[3] > 0.0 { vec![o] } else { Vec::new() }))
    }, 0);
    it.method(p, "scrollIntoView", |it, a| {
        let n = this_node(it, a)?;
        if let Some(r) = it.host.rects.get(&n) {
            it.host.scroll_req = Some((0.0, (r[1] - 8.0).max(0.0)));
        }
        Ok(Value::Undef)
    }, 0);
    for name in ["scrollTo", "scroll", "scrollBy"] {
        it.method(p, name, |_, _| Ok(Value::Undef), 2);
    }
    it.method(p, "focus", |it, a| {
        let n = this_node(it, a)?;
        it.host.focus_req = Some(Some(n));
        it.host.focused = Some(n);
        Ok(Value::Undef)
    }, 0);
    it.method(p, "blur", |it, a| {
        let n = this_node(it, a)?;
        if it.host.focused == Some(n) {
            it.host.focus_req = Some(None);
            it.host.focused = None;
        }
        Ok(Value::Undef)
    }, 0);
    it.method(p, "click", |it, a| {
        let n = this_node(it, a)?;
        click(it, n, false)?;
        Ok(Value::Undef)
    }, 0);
    it.method(p, "submit", |it, a| {
        let n = this_node(it, a)?;
        it.host.submit = Some(n);
        Ok(Value::Undef)
    }, 0);
    it.method(p, "requestSubmit", |it, a| {
        let n = this_node(it, a)?;
        if fire_simple(it, n, "submit")? {
            it.host.submit = Some(n);
        }
        Ok(Value::Undef)
    }, 0);
    it.method(p, "reset", |it, a| {
        let n = this_node(it, a)?;
        for c in form_controls(it, n) {
            it.host.form.values.remove(&c);
            let on = attr(it, c, "checked").is_some();
            if is_checkable(it, c) {
                it.host.form.checked.insert(c, on);
            }
        }
        it.host.repaint = true;
        Ok(Value::Undef)
    }, 0);
    it.method(p, "select", |_, _| Ok(Value::Undef), 0);
    it.method(p, "setSelectionRange", |_, _| Ok(Value::Undef), 2);
    it.method(p, "checkValidity", |_, _| Ok(Value::Bool(true)), 0);
    it.method(p, "reportValidity", |_, _| Ok(Value::Bool(true)), 0);
    it.method(p, "setCustomValidity", |_, _| Ok(Value::Undef), 1);
    it.method(p, "insertAdjacentHTML", |it, a| {
        let n = this_node(it, a)?;
        let pos = str_arg(it, a, 0)?.to_ascii_lowercase();
        let t = str_arg(it, a, 1)?;
        let ctx = if pos == "beforebegin" || pos == "afterend" { it.host.doc.parent(n).map(|p| it.host.doc.name(p).to_string()).unwrap_or_default() } else { it.host.doc.name(n).to_string() };
        let nodes = parse_fragment(it, &ctx, &t)?;
        adjacent(it, n, &pos, nodes)?;
        Ok(Value::Undef)
    }, 2);
    it.method(p, "insertAdjacentElement", |it, a| {
        let n = this_node(it, a)?;
        let pos = str_arg(it, a, 0)?.to_ascii_lowercase();
        let c = arg_node(it, &a.get(1))?;
        adjacent(it, n, &pos, vec![c])?;
        Ok(a.get(1))
    }, 2);
    it.method(p, "insertAdjacentText", |it, a| {
        let n = this_node(it, a)?;
        let pos = str_arg(it, a, 0)?.to_ascii_lowercase();
        let t = str_arg(it, a, 1)?;
        let c = new_node(it, NodeData::Text(t))?;
        adjacent(it, n, &pos, vec![c])?;
        Ok(Value::Undef)
    }, 2);
    it.method(p, "attachShadow", |it, a| {
        // No shadow trees: the element itself stands in for its shadow root.
        Ok(a.this.clone()).map(|v| { let _ = it; v })
    }, 1);
    it.method(p, "animate", |it, _| {
        let o = it.new_object();
        it.method(o, "cancel", |_, _| Ok(Value::Undef), 0);
        it.method(o, "finish", |_, _| Ok(Value::Undef), 0);
        it.method(o, "play", |_, _| Ok(Value::Undef), 0);
        it.method(o, "pause", |_, _| Ok(Value::Undef), 0);
        let p = super::builtins::promise_resolved(it, Value::Undef)?;
        it.set_prop(o, "finished", p);
        it.set_prop(o, "onfinish", Value::Null);
        Ok(Value::Obj(o))
    }, 2);
    it.method(p, "getAnimations", |it, _| Ok(it.new_array(Vec::new())), 0);
    it.method(p, "splitText", |it, a| {
        let n = this_node(it, a)?;
        let at = it.to_num(&a.get(0))?.max(0.0) as usize;
        let NodeData::Text(t) = it.host.doc.nodes[n].data.clone() else { return it.type_err("splitText on a non-text node") };
        let head: String = t.chars().take(at).collect();
        let tail: String = t.chars().skip(at).collect();
        it.host.doc.nodes[n].data = NodeData::Text(head);
        let tn = new_node(it, NodeData::Text(tail))?;
        if let Some(p) = it.host.doc.parent(n) {
            match sibling(it, n, 1, false) {
                Some(nx) => it.host.doc.insert_before(p, tn, nx),
                None => it.host.doc.append(p, tn),
            }
        }
        it.host.dirty = true;
        Ok(wrap(it, tn))
    }, 1);
    it.method(p, "appendData", |it, a| {
        let n = this_node(it, a)?;
        let more = str_arg(it, a, 0)?;
        if let NodeData::Text(t) | NodeData::Comment(t) = &mut it.host.doc.nodes[n].data {
            t.push_str(&more);
            it.host.dirty = true;
        }
        Ok(Value::Undef)
    }, 1);
    // select.add / remove(index) for <select>.
    it.method(p, "add", |it, a| {
        let n = this_node(it, a)?;
        let c = arg_node(it, &a.get(0))?;
        insert(it, n, c, None)?;
        Ok(Value::Undef)
    }, 1);
    it.method(p, "item", |it, a| {
        let n = this_node(it, a)?;
        let i = it.to_num(&a.get(0))? as usize;
        let o = options_of(it, n).get(i).copied();
        Ok(wrap_opt(it, o))
    }, 1);
    it.method(p, "toString", |it, a| {
        let n = this_node(it, a)?;
        Ok(s(&match &it.host.doc.nodes[n].data {
            NodeData::Document => "[object HTMLDocument]".to_string(),
            NodeData::Text(_) => "[object Text]".to_string(),
            NodeData::Element(e) if e.name == "a" => resolved_attr(it, n, "href"),
            NodeData::Element(_) => "[object HTMLElement]".to_string(),
            _ => "[object Node]".to_string(),
        }))
    }, 0);
    for (k, v) in [
        ("ELEMENT_NODE", 1.0),
        ("ATTRIBUTE_NODE", 2.0),
        ("TEXT_NODE", 3.0),
        ("CDATA_SECTION_NODE", 4.0),
        ("PROCESSING_INSTRUCTION_NODE", 7.0),
        ("COMMENT_NODE", 8.0),
        ("DOCUMENT_NODE", 9.0),
        ("DOCUMENT_TYPE_NODE", 10.0),
        ("DOCUMENT_FRAGMENT_NODE", 11.0),
        ("DOCUMENT_POSITION_DISCONNECTED", 1.0),
        ("DOCUMENT_POSITION_PRECEDING", 2.0),
        ("DOCUMENT_POSITION_FOLLOWING", 4.0),
        ("DOCUMENT_POSITION_CONTAINS", 8.0),
        ("DOCUMENT_POSITION_CONTAINED_BY", 16.0),
    ] {
        it.hide(p, k, Value::Num(v));
    }
    // Constructors for instanceof checks; they all share the one prototype.
    let g = it.global;
    let names = [
        "Node", "Element", "HTMLElement", "Document", "HTMLDocument", "Text", "Comment", "CharacterData", "DocumentFragment", "SVGElement",
        "HTMLDivElement", "HTMLSpanElement", "HTMLAnchorElement", "HTMLInputElement", "HTMLButtonElement", "HTMLFormElement", "HTMLSelectElement",
        "HTMLOptionElement", "HTMLTextAreaElement", "HTMLImageElement", "HTMLScriptElement", "HTMLBodyElement", "HTMLHtmlElement", "HTMLHeadElement",
        "HTMLLinkElement", "HTMLStyleElement", "HTMLMetaElement", "HTMLTemplateElement", "HTMLIFrameElement", "HTMLCanvasElement", "HTMLVideoElement",
        "HTMLMediaElement", "HTMLTableElement", "HTMLLabelElement", "HTMLUListElement", "HTMLLIElement", "HTMLParagraphElement", "HTMLHeadingElement",
        "ShadowRoot", "HTMLSlotElement", "HTMLUnknownElement", "HTMLDetailsElement", "HTMLDialogElement",
    ];
    for name in names {
        let c = it.new_native(name, |it, _| it.type_err("Illegal constructor"), 0);
        it.heap.get_mut(c).props.set("prototype".into(), Slot::Data(Value::Obj(p), 0));
        for (k, v) in [("ELEMENT_NODE", 1.0), ("TEXT_NODE", 3.0), ("COMMENT_NODE", 8.0), ("DOCUMENT_NODE", 9.0), ("DOCUMENT_FRAGMENT_NODE", 11.0)] {
            it.hide(c, k, Value::Num(v));
        }
        it.hide(g, name, Value::Obj(c));
        if name == "HTMLElement" {
            it.hide(p, "constructor", Value::Obj(c));
        }
    }

    // classList
    let cl = it.new_object();
    it.p.class_list = Some(cl);
    fn cl_node(it: &mut Interp, a: &A) -> R<NodeId> {
        match &a.this {
            Value::Obj(g) => match it.heap.get(*g).kind {
                Kind::ClassList(n) => Ok(n),
                _ => it.type_err("Illegal invocation"),
            },
            _ => it.type_err("Illegal invocation"),
        }
    }
    fn set_classes(it: &mut Interp, n: NodeId, v: &[String]) {
        let t = v.join(" ");
        set_attr(it, n, "class", &t);
    }
    it.method(cl, "add", |it, a| {
        let n = cl_node(it, a)?;
        let mut v = class_list(it, n);
        for x in a.args {
            let c = it.to_str(x)?.to_string();
            if c.is_empty() || c.contains(char::is_whitespace) {
                return Err(it.err(ErrKind::Syntax, "InvalidCharacterError: class names can't contain spaces"));
            }
            if !v.contains(&c) {
                v.push(c);
            }
        }
        set_classes(it, n, &v);
        Ok(Value::Undef)
    }, 1);
    it.method(cl, "remove", |it, a| {
        let n = cl_node(it, a)?;
        let mut v = class_list(it, n);
        for x in a.args {
            let c = it.to_str(x)?.to_string();
            v.retain(|y| *y != c);
        }
        set_classes(it, n, &v);
        Ok(Value::Undef)
    }, 1);
    it.method(cl, "toggle", |it, a| {
        let n = cl_node(it, a)?;
        let mut v = class_list(it, n);
        let c = it.to_str(&a.get(0))?.to_string();
        let has = v.contains(&c);
        let want = if a.args.len() > 1 { Interp::truthy(&a.get(1)) } else { !has };
        if want && !has {
            v.push(c);
        } else if !want && has {
            v.retain(|y| *y != c);
        }
        set_classes(it, n, &v);
        Ok(Value::Bool(want))
    }, 1);
    it.method(cl, "contains", |it, a| {
        let n = cl_node(it, a)?;
        let c = it.to_str(&a.get(0))?.to_string();
        Ok(Value::Bool(class_list(it, n).contains(&c)))
    }, 1);
    it.method(cl, "replace", |it, a| {
        let n = cl_node(it, a)?;
        let old = it.to_str(&a.get(0))?.to_string();
        let new = it.to_str(&a.get(1))?.to_string();
        let mut v = class_list(it, n);
        let Some(i) = v.iter().position(|x| *x == old) else { return Ok(Value::Bool(false)) };
        v[i] = new;
        set_classes(it, n, &v);
        Ok(Value::Bool(true))
    }, 2);
    it.method(cl, "item", |it, a| {
        let n = cl_node(it, a)?;
        let i = it.to_num(&a.get(0))? as usize;
        Ok(class_list(it, n).get(i).map(|c| s(c)).unwrap_or(Value::Null))
    }, 1);
    it.method(cl, "forEach", |it, a| {
        let n = cl_node(it, a)?;
        let f = a.get(0);
        for (i, c) in class_list(it, n).into_iter().enumerate() {
            it.call(&f, Value::Undef, vec![s(&c), Value::Num(i as f64), a.this.clone()])?;
        }
        Ok(Value::Undef)
    }, 1);
    it.method(cl, "toString", |it, a| {
        let n = cl_node(it, a)?;
        Ok(s(&attr(it, n, "class").unwrap_or_default()))
    }, 0);
    it.method(cl, "@@iterator", |it, a| {
        let n = cl_node(it, a)?;
        let v: Vec<Value> = class_list(it, n).iter().map(|c| s(c)).collect();
        let arr = it.new_array(v);
        super::builtins::make_iter(it, arr, IterKind::Values)
    }, 0);

    // style
    let st = it.new_object();
    it.p.style = Some(st);
    fn st_node(it: &mut Interp, a: &A) -> R<NodeId> {
        match &a.this {
            Value::Obj(g) => match it.heap.get(*g).kind {
                Kind::Style(n) => Ok(n),
                _ => it.type_err("Illegal invocation"),
            },
            _ => it.type_err("Illegal invocation"),
        }
    }
    it.method(st, "setProperty", |it, a| {
        let n = st_node(it, a)?;
        let k = str_arg(it, a, 0)?.to_ascii_lowercase();
        let v = if a.get(1).is_nullish() { String::new() } else { str_arg(it, a, 1)? };
        let imp = str_arg(it, a, 2).unwrap_or_default();
        let v = if imp == "important" && !v.is_empty() { format!("{v} !important") } else { v };
        set_style(it, n, &k, &v);
        Ok(Value::Undef)
    }, 2);
    it.method(st, "getPropertyValue", |it, a| {
        let n = st_node(it, a)?;
        let k = str_arg(it, a, 0)?.to_ascii_lowercase();
        Ok(s(&get_style(it, n, &k)))
    }, 1);
    it.method(st, "getPropertyPriority", |it, a| {
        let n = st_node(it, a)?;
        let k = str_arg(it, a, 0)?.to_ascii_lowercase();
        Ok(s(if get_style(it, n, &k).ends_with("!important") { "important" } else { "" }))
    }, 1);
    it.method(st, "removeProperty", |it, a| {
        let n = st_node(it, a)?;
        let k = str_arg(it, a, 0)?.to_ascii_lowercase();
        let old = get_style(it, n, &k);
        set_style(it, n, &k, "");
        Ok(s(&old))
    }, 1);
    it.method(st, "item", |it, a| {
        let n = st_node(it, a)?;
        let i = it.to_num(&a.get(0))? as usize;
        let d = style_decls(&attr(it, n, "style").unwrap_or_default());
        Ok(s(d.get(i).map(|x| x.0.as_str()).unwrap_or("")))
    }, 1);

    let ds = it.new_object();
    it.p.dataset = Some(ds);
}

fn adjacent(it: &mut Interp, n: NodeId, pos: &str, nodes: Vec<NodeId>) -> R<()> {
    match pos {
        "beforebegin" | "afterend" => {
            let Some(p) = it.host.doc.parent(n) else { return Ok(()) };
            let anchor = if pos == "beforebegin" { Some(n) } else { sibling(it, n, 1, false) };
            for c in nodes {
                insert(it, p, c, anchor)?;
            }
        }
        "afterbegin" => {
            let first = it.host.doc.children(n).first().copied();
            for c in nodes {
                insert(it, n, c, first)?;
            }
        }
        "beforeend" => {
            for c in nodes {
                insert(it, n, c, None)?;
            }
        }
        _ => return Err(it.err(ErrKind::Syntax, "SyntaxError: the position must be beforebegin, afterbegin, beforeend or afterend")),
    }
    Ok(())
}

/// A click on `n` (from the user or from element.click()): the event, and
/// for checkboxes, radios and buttons their default action. Returns
/// whether the default action should happen.
pub fn click(it: &mut Interp, n: NodeId, from_user: bool) -> R<bool> {
    let checkable = is_checkable(it, n);
    let was = it.host.form.checked.get(&n).copied().unwrap_or(false);
    if checkable && !from_user {
        // element.click() toggles first, as a real click does.
        let radio = input_type(it, n) == "radio";
        set_checked(it, n, if radio { true } else { !was });
    }
    let ev = new_event(it, "click", &EventInit { bubbles: true, cancelable: true, client: Some((0.0, 0.0)), ..Default::default() });
    let t = wrap(it, n);
    let ok = dispatch(it, &t, ev)?;
    if checkable && !from_user {
        if !ok {
            it.host.form.checked.insert(n, was);
        } else if was != it.host.form.checked.get(&n).copied().unwrap_or(false) {
            fire_simple(it, n, "input")?;
            fire_simple(it, n, "change")?;
        }
    }
    if ok && !from_user {
        // Default actions a script-initiated click has.
        let name = it.host.doc.name(n).to_string();
        let ty = input_type(it, n);
        let submit = (name == "button" && attr(it, n, "type").map(|t| t.eq_ignore_ascii_case("submit")).unwrap_or(true)) || (name == "input" && (ty == "submit" || ty == "image"));
        if submit {
            if let Some(f) = form_of(it, n) {
                if fire_simple(it, f, "submit")? {
                    it.host.submit = Some(n);
                }
            }
        } else if name == "a" {
            if let Some(h) = attr(it, n, "href") {
                super::window::navigate(it, &h, false);
            }
        }
    }
    Ok(ok)
}

pub fn install(it: &mut Interp) {
    install_events(it);
    install_node(it);
    super::window::install(it);
}
