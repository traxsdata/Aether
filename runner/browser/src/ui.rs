//! ui.rs - the browser window: toolbar (back, forward, reload/stop, the
//! address bar with its padlock), the page view with scrolling, link and
//! form interaction, keyboard focus, a status line, and history.
//!
//! Pages load on a worker thread so Stop works and the window stays live.
//! A page's scripts run on this thread, between events: the window hands
//! them clicks, typing and timers, and applies what they change.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;

use aether_host::{abi, Backend, Event, WinHandle};

use crate::css::Color;
use crate::dom::NodeId;
use crate::font::{Cache, Face};
use crate::layout::{Item, Page, Target};
use crate::net::{FetchError, Request, Url};
use crate::page::{self, Fetcher, LoadError, Loaded, Location};
use crate::paint::{paint_page, Canvas, FormState, Rect};
use crate::pages;
use crate::script::{self, EventInit, Mode, NavReq, Policy, Runtime, Setup};

// æther's palette (kernel/wm.c).
const ACCENT: u32 = 0x38E1F5;
const ACCENT2: u32 = 0xB06CFF;
const GLASS: u32 = 0x0E1428;
const TEXT: u32 = 0xE6ECFF;
const TEXT_DIM: u32 = 0x8C96B8;
const DANGER: u32 = 0xFF5C7A;
const SAFE: u32 = 0x5CF2B0;
const BAR_TOP: u32 = 0x1A2344;
const BAR_BOTTOM: u32 = 0x121933;
const EDGE: u32 = 0x2A3560;

pub const TAB_H: i32 = 36;
pub const TOOLBAR_H: i32 = 52;
/// Keeping every tab's page, scripts and fonts in memory is cheap; an
/// unbounded number of them is not.
pub const MAX_TABS: usize = 32;
const LINE_PX: f32 = 40.0;
const WHEEL_PX: f32 = 54.0;

fn c(x: u32) -> Color {
    Color::hex(x)
}

/// What a history entry (re)loads.
#[derive(Clone, Debug)]
enum Nav {
    About(String),
    Web { url: Url, allow_http: bool, fragment: Option<String> },
    /// A request with a body (a POST form); kept so Reload can repeat it.
    Request { req: Request, fragment: Option<String> },
}

impl Nav {
    fn display(&self) -> String {
        match self {
            Nav::About(n) => format!("about:{n}"),
            Nav::Web { url, fragment, .. } => with_fragment(url.as_str(), fragment),
            Nav::Request { req, .. } => req.url.as_str().to_string(),
        }
    }
}

fn with_fragment(u: &str, f: &Option<String>) -> String {
    match f {
        Some(f) => format!("{u}#{f}"),
        None => u.to_string(),
    }
}

#[derive(Clone, Debug)]
struct Entry {
    nav: Nav,
    scroll: f32,
}

#[derive(Clone, Debug, PartialEq)]
enum Kind {
    Normal,
    /// The plain-http warning for this URL.
    Interstitial(Url, Option<String>),
    Error,
}

/// The page on screen.
struct Shown {
    loaded: Loaded,
    page: Page,
    form: FormState,
    kind: Kind,
    scroll: f32,
    /// Focusable targets in page order, with their first hit rectangle.
    focusables: Vec<(Target, f32, f32, f32, f32)>,
    /// The page's scripts, when it runs any.
    rt: Option<Runtime>,
    /// The focused field and its value when it got focus (for "change").
    focus_value: Option<(NodeId, String)>,
    /// The scroll position scripts last heard about.
    told_scroll: f32,
}

struct Loading {
    rx: Receiver<Result<(Loaded, Page), LoadError>>,
    cancel: Arc<AtomicBool>,
    nav: Nav,
    /// Where it goes in history: push a new entry, or replace the current.
    push: bool,
    started: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    Address,
    Page,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Button {
    Back,
    Forward,
    Reload,
    Scripts,
    Shield,
}

/// One browser tab: its own page, history, scripts and address bar.
pub struct Tab {
    w: i32,
    h: i32,
    cache: Cache,
    shown: Shown,
    history: Vec<Entry>,
    index: usize,
    loading: Option<Loading>,
    focus: Focus,
    /// Keyboard focus within the page (a link or a field).
    page_focus: Option<Target>,
    addr: String,
    addr_caret: usize,
    addr_all: bool,
    hover_button: Option<Button>,
    hover_target: Option<Target>,
    status: String,
    mouse: (i32, i32),
    drag_scroll: Option<(i32, f32)>,
    caret_on: bool,
    http_ok: HashSet<String>,
    sync_loads: bool,
    now: u64,
    pub title: String,
    /// The last event changed nothing on screen.
    pub quiet: bool,
    fetcher: Fetcher<'static>,
    /// Which sites may run scripts (this run of the browser only).
    policy: Policy,
    /// What scripts store (localStorage...), kept in memory only.
    storage: Rc<RefCell<script::Storage>>,
    /// Navigations scripts started since the user last did something.
    script_navs: u32,
    last_tick: u64,
    /// Counts pages shown (so code can tell the page changed under it).
    gen: u64,
    /// A page finished loading in a background tab; its scripts wait until
    /// the tab is selected, so they can't take over the UI thread unseen.
    scripts_pending: bool,
}

fn builtin_shown(name: &str, html: &str, kind: Kind, w: i32, h: i32) -> Shown {
    let loaded = page::load_builtin(name, html);
    let page = page::lay_out(&loaded, w as f32, (h - TOOLBAR_H) as f32);
    make_shown(loaded, page, kind)
}

fn make_shown(loaded: Loaded, page: Page, kind: Kind) -> Shown {
    let form = page::initial_form(&loaded.doc);
    let focusables = focusables(&page);
    Shown { loaded, page, form, kind, scroll: 0.0, focusables, rt: None, focus_value: None, told_scroll: 0.0 }
}

fn focusables(page: &Page) -> Vec<(Target, f32, f32, f32, f32)> {
    let mut out: Vec<(Target, f32, f32, f32, f32)> = Vec::new();
    let mut seen = HashSet::new();
    for it in &page.items {
        if let Item::Hit { x, y, w, h, target } = it {
            if seen.insert(*target) {
                out.push((*target, *x, *y, *w, *h));
            }
        }
    }
    out
}

impl Tab {
    pub fn new(w: i32, h: i32, sync_loads: bool) -> Tab {
        let shown = builtin_shown("home", &pages::home(), Kind::Normal, w, h);
        Tab {
            w,
            h,
            cache: Cache::default(),
            title: shown.loaded.title.clone(),
            shown,
            history: vec![Entry { nav: Nav::About("home".into()), scroll: 0.0 }],
            index: 0,
            loading: None,
            focus: Focus::Address,
            page_focus: None,
            addr: String::new(),
            addr_caret: 0,
            addr_all: false,
            hover_button: None,
            hover_target: None,
            status: String::new(),
            mouse: (-1, -1),
            drag_scroll: None,
            caret_on: true,
            http_ok: HashSet::new(),
            sync_loads,
            now: 0,
            quiet: false,
            fetcher: page::default_fetcher(),
            policy: Policy::default(),
            storage: Rc::default(),
            script_navs: 0,
            last_tick: 0,
            gen: 0,
            scripts_pending: false,
        }
    }

    /// Where pages come from (tests use a local stand-in).
    pub fn set_fetcher(&mut self, f: Fetcher<'static>) {
        self.fetcher = f;
    }

    fn view_h(&self) -> f32 {
        (self.h - TOOLBAR_H).max(0) as f32
    }

    fn max_scroll(&self) -> f32 {
        (self.shown.page.height - self.view_h()).max(0.0)
    }

    fn scroll_to(&mut self, y: f32) {
        self.shown.scroll = y.clamp(0.0, self.max_scroll()).round();
    }

    fn current_nav(&self) -> &Nav {
        &self.history[self.index].nav
    }

    fn address_text(&self) -> String {
        match &self.shown.kind {
            Kind::Interstitial(u, f) => with_fragment(u.as_str(), f),
            _ => {
                if let Some(l) = &self.loading {
                    return l.nav.display();
                }
                let n = self.current_nav().display();
                if n == "about:home" {
                    String::new()
                } else {
                    n
                }
            }
        }
    }

    pub fn is_loading(&self) -> bool {
        self.loading.is_some()
    }

    // ---- navigation ------------------------------------------------------

    /// Opens what the user typed in the address bar.
    pub fn open_typed(&mut self, text: &str) {
        let t = text.trim();
        if t.is_empty() {
            return;
        }
        if let Some(name) = t.strip_prefix("about:") {
            self.go(Nav::About(name.to_ascii_lowercase()), true);
            return;
        }
        let lower = t.to_ascii_lowercase();
        if lower.starts_with("javascript:") {
            self.status = "Scripts can't be run from the address bar.".into();
            return;
        }
        match Url::from_user_input(t) {
            Ok(u) => {
                let (u, frag) = split_fragment(u);
                self.go(Nav::Web { url: u, allow_http: false, fragment: frag }, true);
            }
            Err(e) => self.show_error(t, &e.to_string(), true),
        }
    }

    /// Follows a link's href from the current page.
    fn follow(&mut self, href: &str) {
        let h = href.trim();
        let lower = h.to_ascii_lowercase();
        let internal = !matches!(self.shown.loaded.location, Location::Web(_));
        if let Some(name) = lower.strip_prefix("about:") {
            match name {
                "home" | "security" => self.go(Nav::About(name.into()), true),
                // Interstitial actions only from the interstitial itself.
                "continue" => {
                    if let Kind::Interstitial(u, f) = self.shown.kind.clone() {
                        self.http_ok.insert(u.host().to_string());
                        self.go(Nav::Web { url: u, allow_http: true, fragment: f }, false);
                    }
                }
                "back" => {
                    if matches!(self.shown.kind, Kind::Interstitial(..)) {
                        self.back();
                    }
                }
                _ => {
                    if internal {
                        self.go(Nav::About(name.into()), true);
                    }
                }
            }
            return;
        }
        if let Some(code) = h.get(11..).filter(|_| lower.starts_with("javascript:")) {
            // The page's own code, as its inline scripts are.
            let code = page::percent_decode(code);
            let sh = &mut self.shown;
            match sh.rt.as_mut() {
                Some(rt) => {
                    rt.run_script(&mut sh.loaded.doc, &mut sh.form, &code);
                    self.apply_effects();
                }
                None => self.status = "Scripts are off for this site.".into(),
            }
            return;
        }
        // In-page links.
        if let Some(frag) = h.strip_prefix('#') {
            self.jump_to_fragment(frag);
            return;
        }
        let Some(base) = self.shown.loaded.base.clone() else {
            if let Ok(u) = Url::parse(h) {
                let (u, frag) = split_fragment(u);
                self.go(Nav::Web { url: u, allow_http: false, fragment: frag }, true);
            }
            return;
        };
        match base.join(h) {
            Ok(u) => {
                let (u, frag) = split_fragment(u);
                if let (Some(f), Location::Web(cur)) = (&frag, &self.shown.loaded.location) {
                    if strip_fragment(cur.as_str()) == u.as_str() {
                        let f = f.clone();
                        self.jump_to_fragment(&f);
                        return;
                    }
                }
                let allow = self.shown.loaded.allow_http && u.scheme() == "http";
                self.go(Nav::Web { url: u, allow_http: allow, fragment: frag }, true);
            }
            Err(e) => {
                let scheme = lower.split(':').next().unwrap_or("");
                self.status = if lower.contains(':') && !scheme.is_empty() && scheme.len() < 12 {
                    format!("{scheme}: links aren't supported by this browser")
                } else {
                    format!("{e}")
                };
            }
        }
    }

    fn jump_to_fragment(&mut self, frag: &str) {
        // Scripts see the new address and a hashchange event.
        if let (Location::Web(cur), Some(_)) = (&self.shown.loaded.location, &self.shown.rt) {
            if let Ok(u) = Url::parse(&format!("{}#{frag}", strip_fragment(cur.as_str()))) {
                let sh = &mut self.shown;
                if let Some(rt) = sh.rt.as_mut() {
                    rt.set_url(u);
                    rt.fire_window(&mut sh.loaded.doc, &mut sh.form, "hashchange");
                }
                self.apply_effects();
            }
        }
        match page::fragment_target(&self.shown.loaded.doc, frag).and_then(|n| self.shown.page.anchor_y(n)) {
            Some(y) => self.scroll_to(y - 8.0),
            None if frag.is_empty() || frag.eq_ignore_ascii_case("top") => self.scroll_to(0.0),
            None => {}
        }
    }

    fn go(&mut self, nav: Nav, push: bool) {
        self.stop();
        // Remember where we were.
        self.history[self.index].scroll = self.shown.scroll;
        match nav.clone() {
            Nav::About(name) => match pages::about(&name) {
                Some(html) => {
                    let shown = builtin_shown(&name, &html, Kind::Normal, self.w, self.h);
                    self.commit(nav, shown, push);
                }
                None => self.show_error(&format!("about:{name}"), "There is no built-in page with that name.", push),
            },
            Nav::Web { url, allow_http, fragment } => {
                if url.scheme() == "http" && !allow_http && !self.http_ok.contains(url.host()) {
                    self.interstitial(url, fragment, push);
                    return;
                }
                let mut req = Request::get(url);
                req.allow_http = allow_http || req.url.scheme() == "http";
                self.start_load(req, fragment, nav, push);
            }
            Nav::Request { req, fragment } => self.start_load(req, fragment, nav, push),
        }
    }

    fn interstitial(&mut self, url: Url, fragment: Option<String>, push: bool) {
        let html = pages::http_warning(&with_fragment(url.as_str(), &fragment));
        let shown = builtin_shown("warning", &html, Kind::Interstitial(url.clone(), fragment.clone()), self.w, self.h);
        // History keeps the address (going back here shows the warning again).
        self.commit(Nav::Web { url, allow_http: false, fragment }, shown, push);
        self.focus = Focus::Page;
    }

    fn show_error(&mut self, what: &str, msg: &str, push: bool) {
        let shown = builtin_shown("error", &pages::error(what, msg), Kind::Error, self.w, self.h);
        let nav = match Url::from_user_input(what) {
            Ok(u) if !what.starts_with("about:") => Nav::Web { url: u, allow_http: false, fragment: None },
            _ => Nav::About("home".into()),
        };
        self.commit(nav, shown, push);
    }

    fn commit(&mut self, nav: Nav, shown: Shown, push: bool) {
        if push {
            self.history.truncate(self.index + 1);
            self.history.push(Entry { nav, scroll: 0.0 });
            if self.history.len() > 200 {
                self.history.remove(0);
            }
            self.index = self.history.len() - 1;
        } else {
            self.history[self.index].nav = nav;
        }
        self.title = shown.loaded.title.clone();
        self.shown = shown;
        self.scripts_pending = false;
        self.gen += 1;
        self.page_focus = None;
        self.hover_target = None;
        if self.focus == Focus::Address && !self.addr.is_empty() {
            self.focus = Focus::Page;
        }
        if !matches!(self.shown.loaded.location, Location::About(ref n) if n == "home") {
            self.focus = Focus::Page;
        }
        self.addr = self.address_text();
        self.addr_caret = self.addr.len();
        self.addr_all = false;
    }

    fn start_load(&mut self, req: Request, fragment: Option<String>, nav: Nav, push: bool) {
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel();
        let (w, h) = (self.w as f32, self.view_h());
        let flag = cancel.clone();
        let host = req.url.host().to_string();
        let fetcher = self.fetcher;
        let policy = self.policy.clone();
        let spawned = std::thread::Builder::new().name("load".into()).stack_size(32 << 20).spawn(move || {
            let r = page::load(req, fragment, fetcher, &flag, &policy).map(|l| {
                let p = page::lay_out(&l, w, h);
                (l, p)
            });
            let _ = tx.send(r);
        });
        if spawned.is_err() {
            self.show_error(&nav.display(), "the browser couldn't start loading (out of memory?)", push);
            return;
        }
        self.status = format!("Connecting to {host}\u{2026}");
        self.loading = Some(Loading { rx, cancel, nav, push, started: self.now });
        self.addr = self.address_text();
        self.addr_caret = self.addr.len();
        self.focus = Focus::Page;
        if self.sync_loads {
            // Headless runs are scripted: wait for the page.
            let r = self.loading.as_ref().unwrap().rx.recv();
            self.finish_load(r.unwrap_or(Err(LoadError::Cancelled)), true);
        }
    }

    /// Checks on a load in progress; true if something changed.
    pub fn poll(&mut self) -> bool {
        let Some(l) = &self.loading else { return self.tick_scripts() };
        match l.rx.try_recv() {
            Ok(r) => {
                self.finish_load(r, true);
                true
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => true, // animate the progress bar
            Err(_) => {
                self.finish_load(Err(LoadError::Cancelled), true);
                true
            }
        }
    }

    /// A tab that isn't on screen still finishes loading, but its timers wait
    /// until it is selected (background tabs are throttled, not forgotten).
    pub fn poll_background(&mut self) -> bool {
        let Some(l) = &self.loading else { return false };
        match l.rx.try_recv() {
            Ok(r) => {
                self.finish_load(r, false);
                true
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => false,
            Err(_) => {
                self.finish_load(Err(LoadError::Cancelled), false);
                true
            }
        }
    }

    fn finish_load(&mut self, r: Result<(Loaded, Page), LoadError>, run_scripts: bool) {
        let Some(l) = self.loading.take() else { return };
        self.status.clear();
        match r {
            Ok((loaded, pg)) => {
                // Redirected to a different address: show where we ended up.
                let mut nav = l.nav.clone();
                if let (Location::Web(u), Nav::Web { url, allow_http, fragment }) = (&loaded.location, &l.nav) {
                    if u != url {
                        nav = Nav::Web { url: u.clone(), allow_http: *allow_http, fragment: fragment.clone() };
                    }
                }
                if let Nav::Request { req, fragment } = &l.nav {
                    if req.method != crate::net::Method::Get {
                        // Don't repeat a form submission from history.
                        if let Location::Web(u) = &loaded.location {
                            nav = Nav::Web { url: u.clone(), allow_http: req.allow_http, fragment: fragment.clone() };
                        }
                    }
                }
                if !loaded.blocked.is_empty() {
                    self.status = format!("{} page resource(s) not loaded", loaded.blocked.len());
                }
                let frag = loaded.fragment.clone();
                let shown = make_shown(loaded, pg, Kind::Normal);
                self.commit(nav, shown, l.push);
                let before = self.page_id();
                if run_scripts {
                    self.start_scripts();
                    if self.loading.is_some() || before != self.page_id() {
                        // A script sent the page somewhere else.
                        return;
                    }
                } else {
                    self.scripts_pending = true;
                }
                if let Some(f) = frag {
                    self.jump_to_fragment(&f);
                }
                if !l.push {
                    // Back/forward/reload: restore the scroll position.
                    let s = self.history[self.index].scroll;
                    if s > 0.0 {
                        self.scroll_to(s);
                    }
                }
            }
            Err(LoadError::Cancelled) => {
                self.addr = self.address_text();
            }
            Err(LoadError::Fetch(FetchError::InsecureHttp(_))) => {
                if let Nav::Web { url, fragment, .. } = l.nav {
                    self.interstitial(url, fragment, l.push);
                }
            }
            Err(e) => {
                let what = l.nav.display();
                let shown = builtin_shown("error", &pages::error(&what, &sentence(&e.to_string())), Kind::Error, self.w, self.h);
                self.commit(l.nav, shown, l.push);
            }
        }
    }

    pub fn stop(&mut self) {
        if let Some(l) = self.loading.take() {
            l.cancel.store(true, Ordering::Relaxed);
            self.status = "Stopped".into();
            self.addr = self.address_text();
            self.addr_caret = self.addr.len();
        }
    }

    pub fn back(&mut self) {
        if self.index > 0 {
            self.stop();
            self.history[self.index].scroll = self.shown.scroll;
            self.index -= 1;
            let nav = self.history[self.index].nav.clone();
            self.go_history(nav);
        }
    }

    pub fn forward(&mut self) {
        if self.index + 1 < self.history.len() {
            self.stop();
            self.history[self.index].scroll = self.shown.scroll;
            self.index += 1;
            let nav = self.history[self.index].nav.clone();
            self.go_history(nav);
        }
    }

    pub fn reload(&mut self) {
        let nav = self.current_nav().clone();
        self.history[self.index].scroll = self.shown.scroll;
        self.go_history(nav);
    }

    fn go_history(&mut self, nav: Nav) {
        let scroll = self.history[self.index].scroll;
        self.go(nav, false);
        if self.loading.is_none() {
            self.scroll_to(scroll);
        }
    }

    // ---- scripts -------------------------------------------------------------

    fn page_id(&self) -> u64 {
        self.gen
    }

    /// Runs scripts that were deferred while the tab was in the background.
    pub fn start_pending_scripts(&mut self) {
        if self.scripts_pending && self.loading.is_none() {
            self.scripts_pending = false;
            self.start_scripts();
        }
    }

    /// Starts the page's scripts (if its site runs any).
    fn start_scripts(&mut self) {
        self.scripts_pending = false;
        let Location::Web(url) = self.shown.loaded.location.clone() else { return };
        if !self.shown.loaded.script_mode.runs() || self.shown.kind != Kind::Normal {
            return;
        }
        let scripts = std::mem::take(&mut self.shown.loaded.scripts);
        let viewport = (self.w as f32, self.view_h());
        let mut rt = Runtime::new(Setup {
            url,
            base: self.shown.loaded.base.clone(),
            third_party: self.shown.loaded.script_mode == Mode::All,
            fetcher: Some(self.fetcher),
            storage: self.storage.clone(),
            viewport,
        });
        rt.set_layout(&self.shown.page, 0.0, viewport);
        let sh = &mut self.shown;
        rt.run_page(&mut sh.loaded.doc, &mut sh.form, &scripts);
        if self.sync_loads {
            // Headless runs are scripted: let the page settle first.
            rt.settle(&mut sh.loaded.doc, &mut sh.form, 2000.0);
        }
        sh.rt = Some(rt);
        self.apply_effects();
    }

    /// Fires an event at a node of the page; false if the page cancelled it.
    fn fire(&mut self, n: NodeId, ty: &str, init: EventInit) -> bool {
        let sh = &mut self.shown;
        match sh.rt.as_mut() {
            Some(rt) => rt.fire(&mut sh.loaded.doc, &mut sh.form, n, ty, init),
            None => true,
        }
    }

    /// Lays the page out again after scripts changed it.
    fn relayout(&mut self) {
        self.shown.page = page::lay_out(&self.shown.loaded, self.w as f32, self.view_h());
        self.shown.focusables = focusables(&self.shown.page);
        let s = self.shown.scroll;
        self.scroll_to(s);
        let t = self.shown.loaded.doc.title();
        if t != self.shown.loaded.title {
            self.shown.loaded.title = t.clone();
            self.title = t;
        }
        let viewport = (self.w as f32, self.view_h());
        if let Some(rt) = self.shown.rt.as_mut() {
            rt.set_layout(&self.shown.page, self.shown.scroll, viewport);
        }
    }

    /// Applies what the page's scripts asked for. True if the window changed.
    fn apply_effects(&mut self) -> bool {
        let Some(rt) = self.shown.rt.as_mut() else { return false };
        let fx = rt.take_effects();
        let mut changed = fx.repaint;
        if fx.relayout {
            self.relayout();
            changed = true;
        }
        if let Some((u, replace)) = fx.url_changed {
            self.shown.loaded.location = Location::Web(u.clone());
            self.shown.loaded.base = Some(u.clone());
            let (bare, frag) = split_fragment(u);
            let nav = Nav::Web { url: bare, allow_http: self.shown.loaded.allow_http, fragment: frag };
            if replace {
                self.history[self.index].nav = nav;
            } else {
                self.history[self.index].scroll = self.shown.scroll;
                self.history.truncate(self.index + 1);
                self.history.push(Entry { nav, scroll: 0.0 });
                if self.history.len() > 200 {
                    self.history.remove(0);
                }
                self.index = self.history.len() - 1;
            }
            if self.focus != Focus::Address {
                self.addr = self.address_text();
                self.addr_caret = self.addr.len();
            }
            changed = true;
        }
        if let Some(f) = fx.focus {
            match f {
                Some(n) if self.shown.focusables.iter().any(|t| t.0 == Target::Field(n)) => {
                    self.focus = Focus::Page;
                    self.page_focus = Some(Target::Field(n));
                    self.shown.form.focus = Some(n);
                    self.shown.focus_value = Some((n, self.shown.form.values.get(&n).cloned().unwrap_or_default()));
                }
                Some(_) => {}
                None => {
                    self.page_focus = None;
                    self.shown.form.focus = None;
                    self.shown.focus_value = None;
                }
            }
            changed = true;
        }
        if let Some((_, y)) = fx.scroll {
            self.scroll_to(y);
            changed = true;
        }
        if let Some(a) = fx.alerts.last() {
            let a: String = a.split_whitespace().collect::<Vec<_>>().join(" ");
            self.status = format!("The page says: {}", a.chars().take(200).collect::<String>());
            changed = true;
        }
        if let Some(n) = fx.submit {
            self.submit_now(n);
            return true;
        }
        if let Some(nav) = fx.nav {
            self.script_navs += 1;
            if self.script_navs > 8 {
                self.status = "The page kept navigating by itself; that was stopped.".into();
                return true;
            }
            match nav {
                NavReq::Go { url, replace } => self.script_go(&url, replace),
                NavReq::Reload => self.reload(),
                NavReq::Back => self.back(),
                NavReq::Forward => self.forward(),
            }
            return true;
        }
        changed
    }

    /// Goes where a script sent the page.
    fn script_go(&mut self, url: &str, replace: bool) {
        let Ok(u) = Url::parse(url) else { return };
        let (u, frag) = split_fragment(u);
        if let (Some(f), Location::Web(cur)) = (&frag, &self.shown.loaded.location) {
            if strip_fragment(cur.as_str()) == u.as_str() {
                let f = f.clone();
                self.jump_to_fragment(&f);
                return;
            }
        }
        let allow = self.shown.loaded.allow_http && u.scheme() == "http";
        self.go(Nav::Web { url: u, allow_http: allow, fragment: frag }, !replace);
    }

    /// Keyboard focus moves to field `n` (or off the fields): blur, change
    /// and focus events.
    fn field_focus(&mut self, n: Option<NodeId>) {
        let prev = self.shown.focus_value.take();
        if let Some(rt) = self.shown.rt.as_mut() {
            rt.set_focus(n);
        }
        if self.shown.rt.is_none() {
            self.shown.focus_value = n.map(|n| (n, String::new()));
            return;
        }
        if let Some((p, old)) = prev {
            if Some(p) == n {
                self.shown.focus_value = Some((p, old));
                return;
            }
            let now = self.shown.form.values.get(&p).cloned().unwrap_or_default();
            if now != old {
                self.fire(p, "change", EventInit { bubbles: true, ..Default::default() });
            }
            self.fire(p, "blur", EventInit::default());
            self.fire(p, "focusout", EventInit { bubbles: true, ..Default::default() });
        }
        if let Some(n) = n {
            let v = self.shown.form.values.get(&n).cloned().unwrap_or_default();
            self.shown.focus_value = Some((n, v));
            self.fire(n, "focus", EventInit::default());
            self.fire(n, "focusin", EventInit { bubbles: true, ..Default::default() });
        }
        self.apply_effects();
    }

    /// Cycles this site's scripts: own only -> all -> off, and reloads.
    fn toggle_scripts(&mut self) {
        let Location::Web(u) = self.shown.loaded.location.clone() else { return };
        let next = match self.policy.mode_for(&u) {
            Mode::Site => Mode::All,
            Mode::All => Mode::Off,
            Mode::Off => Mode::Site,
        };
        self.policy.set(&u, next);
        self.reload();
        self.status = scripts_label(next, 0).into();
    }

    /// Milliseconds until the page's scripts next need to run.
    pub fn script_wake(&self) -> Option<u32> {
        let rt = self.shown.rt.as_ref()?;
        rt.next_due().map(|ms| (ms.ceil() as u32).clamp(15, 60_000))
    }

    /// Script errors on this page so far (for tests and the status line).
    pub fn script_errors(&self) -> Vec<String> {
        self.shown.rt.as_ref().map(|rt| rt.errors().to_vec()).unwrap_or_default()
    }

    /// Runs due timers and finished requests; true if the window changed.
    fn tick_scripts(&mut self) -> bool {
        if self.shown.rt.is_none() || self.loading.is_some() {
            return false;
        }
        if self.now.saturating_sub(self.last_tick) < 15 && self.last_tick != 0 {
            return false;
        }
        self.last_tick = self.now.max(1);
        let scroll = self.shown.scroll;
        let sh = &mut self.shown;
        let Some(rt) = sh.rt.as_mut() else { return false };
        if sh.told_scroll != scroll {
            sh.told_scroll = scroll;
            rt.set_scroll(scroll);
            rt.fire_window(&mut sh.loaded.doc, &mut sh.form, "scroll");
        }
        let ran = rt.tick(&mut sh.loaded.doc, &mut sh.form);
        self.apply_effects() || ran && self.shown.rt.is_none()
    }

    // ---- forms --------------------------------------------------------------

    /// A submission the user started: the form's submit event, then the request.
    fn submit(&mut self, from: NodeId) {
        if self.shown.rt.is_some() {
            if let Some(f) = page::form_of(&self.shown.loaded.doc, from) {
                let ok = self.fire(f, "submit", EventInit { bubbles: true, cancelable: true, ..Default::default() });
                let before = self.page_id();
                self.apply_effects();
                if !ok || before != self.page_id() {
                    return;
                }
            }
        }
        self.submit_now(from);
    }

    fn submit_now(&mut self, from: NodeId) {
        if let Some(req) = page::submit_form(&self.shown.loaded, &self.shown.form, from) {
            if req.method == crate::net::Method::Get {
                let (u, f) = split_fragment(req.url.clone());
                let allow = req.allow_http;
                self.go(Nav::Web { url: u, allow_http: allow, fragment: f }, true);
            } else {
                if req.url.scheme() == "http" && !req.allow_http {
                    self.status = "This form would send your data over plain http; it wasn't sent.".into();
                    return;
                }
                self.go(Nav::Request { req, fragment: None }, true);
            }
        }
    }

    fn toggle(&mut self, node: NodeId) {
        let doc = &self.shown.loaded.doc;
        let Some(e) = doc.element(node) else { return };
        let radio = e.attr("type").is_some_and(|t| t.eq_ignore_ascii_case("radio"));
        let on = self.shown.form.checked.get(&node).copied().unwrap_or(false);
        if radio {
            let name = e.attr("name").unwrap_or("").to_string();
            if !name.is_empty() {
                let group: Vec<NodeId> = (0..doc.nodes.len())
                    .filter(|&n| doc.is(n, "input") && doc.element(n).is_some_and(|x| x.attr("name") == Some(name.as_str()) && x.attr("type").is_some_and(|t| t.eq_ignore_ascii_case("radio"))))
                    .collect();
                for g in group {
                    self.shown.form.checked.insert(g, false);
                }
            }
            self.shown.form.checked.insert(node, true);
        } else {
            self.shown.form.checked.insert(node, !on);
        }
    }

    fn activate(&mut self, t: Target) {
        match t {
            Target::Link(i) => {
                if let Some(h) = self.shown.page.links.get(i as usize).cloned() {
                    self.follow(&h);
                }
            }
            Target::Field(n) => {
                self.focus = Focus::Page;
                self.page_focus = Some(t);
                self.shown.form.focus = Some(n);
                self.field_focus(Some(n));
            }
            Target::Submit(n) => self.submit(n),
            Target::Check(n) => {
                self.page_focus = Some(t);
                self.toggle(n);
            }
        }
    }

    fn describe(&self, t: Target) -> String {
        match t {
            Target::Link(i) => {
                let h = self.shown.page.links.get(i as usize).cloned().unwrap_or_default();
                let lower = h.trim().to_ascii_lowercase();
                if lower.starts_with("javascript:") {
                    return if self.shown.rt.is_some() { "Runs a script on this page".into() } else { "Script link (scripts are off for this site)".into() };
                }
                if lower.starts_with("about:") {
                    return h.trim().to_string();
                }
                match self.shown.loaded.base.as_ref().map(|b| b.join(&h)) {
                    Some(Ok(u)) => u.as_str().to_string(),
                    _ => h,
                }
            }
            Target::Submit(_) => "Submit form".into(),
            Target::Field(_) | Target::Check(_) => String::new(),
        }
    }

    // ---- input --------------------------------------------------------------

    /// Handles one event; returns false when the window should close.
    pub fn handle(&mut self, ev: Event) -> bool {
        self.quiet = false;
        match ev {
            Event::Close => return false,
            Event::Key { key, mods } => self.key(key, mods),
            Event::MouseMove(m) => {
                let before = (self.hover_button, self.hover_target, self.status.len(), self.shown.scroll, self.mouse.1 > self.h - 40);
                self.mouse_move(m.x, m.y, m.buttons);
                let after = (self.hover_button, self.hover_target, self.status.len(), self.shown.scroll, self.mouse.1 > self.h - 40);
                self.quiet = before == after;
            }
            Event::MouseDown(m) => self.mouse_down(m.x, m.y, m.buttons),
            Event::MouseUp(_) => self.drag_scroll = None,
            Event::Scroll { dy, .. } => {
                if self.mouse.1 >= TOOLBAR_H || self.mouse.1 < 0 {
                    let s = self.shown.scroll - dy as f32 * WHEEL_PX;
                    self.scroll_to(s);
                }
            }
            Event::Resize { width, height } => self.resize(width as i32, height as i32),
            Event::Focus | Event::Unfocus => {}
        }
        true
    }

    pub fn resize(&mut self, w: i32, h: i32) {
        if w == self.w && h == self.h {
            return;
        }
        self.w = w;
        self.h = h;
        let frac = if self.max_scroll() > 0.0 { self.shown.scroll / self.max_scroll() } else { 0.0 };
        self.shown.page = page::lay_out(&self.shown.loaded, w as f32, self.view_h());
        self.shown.focusables = focusables(&self.shown.page);
        let s = frac * self.max_scroll();
        self.scroll_to(s);
        let viewport = (w as f32, self.view_h());
        let sh = &mut self.shown;
        if let Some(rt) = sh.rt.as_mut() {
            rt.set_layout(&sh.page, sh.scroll, viewport);
            rt.fire_window(&mut sh.loaded.doc, &mut sh.form, "resize");
            self.apply_effects();
        }
    }

    fn key(&mut self, key: u32, mods: u32) {
        let ctrl = mods & abi::MOD_CTRL != 0;
        let alt = mods & abi::MOD_ALT != 0;
        let shift = mods & abi::MOD_SHIFT != 0;
        self.caret_on = true;
        // Global shortcuts.
        if ctrl && (key == 'l' as u32 || key == 'L' as u32 || key == 12) {
            self.focus_address();
            return;
        }
        if ctrl && (key == 'r' as u32 || key == 'R' as u32 || key == 18) {
            self.reload();
            return;
        }
        if alt && key == abi::KEY_LEFT {
            self.back();
            return;
        }
        if alt && key == abi::KEY_RIGHT {
            self.forward();
            return;
        }
        if key == 27 && self.loading.is_some() {
            self.stop();
            return;
        }
        if self.focus == Focus::Address {
            self.address_key(key, ctrl);
            return;
        }
        self.script_navs = 0;
        // A focused text field takes typing (after the page has seen the key).
        if let Some(Target::Field(n)) = self.page_focus {
            if self.shown.rt.is_some() {
                let init = key_init(key, mods);
                if !self.fire(n, "keydown", init.clone()) {
                    self.apply_effects();
                    return;
                }
                let before = self.shown.form.values.get(&n).cloned();
                let gen = self.gen;
                let used = self.field_key(n, key, ctrl, shift);
                if self.gen != gen {
                    return;
                }
                if used && self.shown.form.values.get(&n).cloned() != before {
                    self.fire(n, "input", EventInit { bubbles: true, ..Default::default() });
                }
                self.fire(n, "keyup", init);
                self.apply_effects();
                if used {
                    return;
                }
            } else if self.field_key(n, key, ctrl, shift) {
                return;
            }
        }
        // Other keys go to the page first; it may take them for itself.
        if self.shown.rt.is_some() {
            let target = self.shown.loaded.doc.body().unwrap_or(0);
            let init = key_init(key, mods);
            let ok = self.fire(target, "keydown", init.clone());
            self.fire(target, "keyup", init);
            let gen = self.gen;
            self.apply_effects();
            if !ok || gen != self.gen {
                return;
            }
        }
        let vh = self.view_h();
        match key {
            abi::KEY_DOWN => self.scroll_to(self.shown.scroll + LINE_PX),
            abi::KEY_UP => self.scroll_to(self.shown.scroll - LINE_PX),
            abi::KEY_PGDN => self.scroll_to(self.shown.scroll + vh * 0.9),
            abi::KEY_PGUP => self.scroll_to(self.shown.scroll - vh * 0.9),
            abi::KEY_HOME => self.scroll_to(0.0),
            abi::KEY_END => self.scroll_to(f32::MAX),
            k if k == ' ' as u32 => {
                let d = if shift { -vh * 0.9 } else { vh * 0.9 };
                self.scroll_to(self.shown.scroll + d);
            }
            8 => self.back(),
            k if k == '\t' as u32 => self.tab(shift),
            k if k == '\n' as u32 => {
                if let Some(t) = self.page_focus {
                    self.activate(t);
                }
            }
            27 => {
                self.page_focus = None;
                self.shown.form.focus = None;
            }
            _ => {}
        }
    }

    /// Typing into a focused text field. Returns true if the key was used.
    fn field_key(&mut self, n: NodeId, key: u32, ctrl: bool, shift: bool) -> bool {
        let textarea = self.shown.loaded.doc.is(n, "textarea");
        let v = self.shown.form.values.entry(n).or_default();
        match key {
            8 => {
                v.pop();
            }
            k if k == '\n' as u32 => {
                if textarea {
                    v.push('\n');
                } else {
                    self.submit(n);
                }
            }
            k if k == '\t' as u32 => {
                self.tab(shift);
            }
            27 => {
                self.page_focus = None;
                self.shown.form.focus = None;
            }
            k if (0x20..0x7F).contains(&k) && !ctrl => {
                if v.len() < 4096 {
                    v.push(char::from_u32(k).unwrap_or(' '));
                }
            }
            _ => return false,
        }
        true
    }

    fn tab(&mut self, back: bool) {
        let n = self.shown.focusables.len();
        if n == 0 {
            return;
        }
        let cur = self.page_focus.and_then(|t| self.shown.focusables.iter().position(|f| f.0 == t));
        let next = match (cur, back) {
            (None, false) => 0,
            (None, true) => n - 1,
            (Some(i), false) => (i + 1) % n,
            (Some(i), true) => (i + n - 1) % n,
        };
        let (t, _, y, _, h) = self.shown.focusables[next];
        self.page_focus = Some(t);
        self.shown.form.focus = if let Target::Field(n) = t { Some(n) } else { None };
        let f = self.shown.form.focus;
        self.field_focus(f);
        self.status = self.describe(t);
        // Keep it in view.
        let vh = self.view_h();
        if y < self.shown.scroll || y + h > self.shown.scroll + vh {
            self.scroll_to(y - vh / 3.0);
        }
    }

    fn focus_address(&mut self) {
        self.focus = Focus::Address;
        self.addr = self.address_text();
        self.addr_caret = self.addr.len();
        self.addr_all = true;
        self.page_focus = None;
        self.shown.form.focus = None;
    }

    fn address_key(&mut self, key: u32, ctrl: bool) {
        let len = self.addr.len();
        match key {
            k if k == '\n' as u32 => {
                let t = self.addr.clone();
                self.addr_all = false;
                self.open_typed(&t);
            }
            27 => {
                self.addr = self.address_text();
                self.addr_caret = self.addr.len();
                self.addr_all = false;
                self.focus = Focus::Page;
            }
            8 => {
                if self.addr_all {
                    self.addr.clear();
                    self.addr_caret = 0;
                } else if self.addr_caret > 0 {
                    self.addr_caret -= 1;
                    self.addr.remove(self.addr_caret);
                }
                self.addr_all = false;
            }
            abi::KEY_DELETE => {
                if self.addr_all {
                    self.addr.clear();
                    self.addr_caret = 0;
                } else if self.addr_caret < len {
                    self.addr.remove(self.addr_caret);
                }
                self.addr_all = false;
            }
            abi::KEY_LEFT => {
                self.addr_caret = if self.addr_all { 0 } else { self.addr_caret.saturating_sub(1) };
                self.addr_all = false;
            }
            abi::KEY_RIGHT => {
                self.addr_caret = if self.addr_all { len } else { (self.addr_caret + 1).min(len) };
                self.addr_all = false;
            }
            abi::KEY_HOME => {
                self.addr_caret = 0;
                self.addr_all = false;
            }
            abi::KEY_END => {
                self.addr_caret = len;
                self.addr_all = false;
            }
            k if k == '\t' as u32 => {
                self.focus = Focus::Page;
                self.addr_all = false;
            }
            k if ctrl && (k == 'a' as u32 || k == 1) => self.addr_all = true,
            k if (0x20..0x7F).contains(&k) && !ctrl => {
                if self.addr_all {
                    self.addr.clear();
                    self.addr_caret = 0;
                    self.addr_all = false;
                }
                if self.addr.len() < 4000 {
                    self.addr.insert(self.addr_caret, char::from_u32(k).unwrap_or(' '));
                    self.addr_caret += 1;
                }
            }
            _ => {}
        }
    }

    // ---- pointer -----------------------------------------------------------

    fn button_rects(&self) -> [(Button, Rect); 5] {
        [
            (Button::Back, Rect::new(10, 10, 32, 32)),
            (Button::Forward, Rect::new(46, 10, 32, 32)),
            (Button::Reload, Rect::new(82, 10, 32, 32)),
            (Button::Scripts, Rect::new(self.w - 80, 10, 32, 32)),
            (Button::Shield, Rect::new(self.w - 44, 10, 32, 32)),
        ]
    }

    fn address_rect(&self) -> Rect {
        Rect::new(124, 9, (self.w - 124 - 90).max(40), 34)
    }

    /// This page's scripts setting (None: a page that never runs scripts).
    fn script_mode(&self) -> Option<Mode> {
        match (&self.shown.loaded.location, &self.shown.kind) {
            (Location::Web(u), Kind::Normal) => Some(self.policy.mode_for(u)),
            _ => None,
        }
    }

    fn scrollbar(&self) -> Option<(Rect, Rect)> {
        let vh = self.view_h();
        let total = self.shown.page.height;
        if total <= vh + 1.0 {
            return None;
        }
        let track = Rect::new(self.w - 12, TOOLBAR_H + 2, 10, vh as i32 - 4);
        let th = ((vh / total) * track.h as f32).max(28.0);
        let ty = track.y as f32 + (self.shown.scroll / self.max_scroll()) * (track.h as f32 - th);
        Some((track, Rect::new(track.x + 2, ty.round() as i32, 6, th.round() as i32)))
    }

    fn mouse_move(&mut self, x: i32, y: i32, buttons: u32) {
        self.mouse = (x, y);
        if let Some((y0, s0)) = self.drag_scroll {
            if buttons & 1 != 0 {
                if let Some((track, thumb)) = self.scrollbar() {
                    let room = (track.h - thumb.h).max(1) as f32;
                    let s = s0 + (y - y0) as f32 / room * self.max_scroll();
                    self.scroll_to(s);
                }
                return;
            }
            self.drag_scroll = None;
        }
        self.hover_button = self.button_rects().iter().find(|(_, r)| r.contains(x, y)).map(|(b, _)| *b);
        let t = if y >= TOOLBAR_H { self.shown.page.hit(x as f32, y as f32 - TOOLBAR_H as f32 + self.shown.scroll) } else { None };
        if t != self.hover_target {
            self.hover_target = t;
            if self.loading.is_none() {
                self.status = t.map(|t| self.describe(t)).unwrap_or_default();
            }
        }
        if let Some(b) = self.hover_button {
            if self.loading.is_none() {
                self.status = match b {
                    Button::Back => "Back",
                    Button::Forward => "Forward",
                    Button::Reload => "Reload",
                    Button::Shield => "Security and privacy",
                    Button::Scripts => match self.script_mode() {
                        Some(m) => scripts_label(m, self.shown.rt.as_ref().map_or(0, |rt| rt.errors().len())),
                        None => "Scripts don't run on this page",
                    },
                }
                .into();
            }
        }
    }

    fn mouse_down(&mut self, x: i32, y: i32, buttons: u32) {
        self.mouse = (x, y);
        if buttons & 1 == 0 {
            return;
        }
        if let Some((b, _)) = self.button_rects().iter().find(|(_, r)| r.contains(x, y)) {
            match b {
                Button::Back => self.back(),
                Button::Forward => self.forward(),
                Button::Reload => {
                    if self.loading.is_some() {
                        self.stop()
                    } else {
                        self.reload()
                    }
                }
                Button::Shield => self.go(Nav::About("security".into()), true),
                Button::Scripts => self.toggle_scripts(),
            }
            return;
        }
        if self.address_rect().contains(x, y) {
            if self.focus != Focus::Address {
                self.focus_address();
            } else {
                // Place the caret.
                let face = Face::new(15.0, false, false, false);
                let r = self.address_rect();
                let mut px = (r.x + 40) as f32;
                let mut pos = self.addr.len();
                for (i, ch) in self.addr.char_indices() {
                    let a = face.advance(ch);
                    if (x as f32) < px + a / 2.0 {
                        pos = i;
                        break;
                    }
                    px += a;
                }
                self.addr_caret = pos;
                self.addr_all = false;
            }
            return;
        }
        if y < TOOLBAR_H {
            return;
        }
        if let Some((track, thumb)) = self.scrollbar() {
            if x >= track.x - 2 {
                if !thumb.contains(x.max(thumb.x), y) {
                    // Jump so the thumb centres on the pointer.
                    let room = (track.h - thumb.h).max(1) as f32;
                    let s = (y - track.y - thumb.h / 2) as f32 / room * self.max_scroll();
                    self.scroll_to(s);
                }
                self.drag_scroll = Some((y, self.shown.scroll));
                return;
            }
        }
        self.focus = Focus::Page;
        self.addr = self.address_text();
        self.addr_all = false;
        self.script_navs = 0;
        let (px, py) = (x as f32, (y - TOOLBAR_H) as f32 + self.shown.scroll);
        let t = self.shown.page.hit(px, py);
        if self.shown.rt.is_some() {
            // The page hears the click first, and can cancel what it would do.
            let node = self.shown.page.node_at(px, py).or(match t {
                Some(Target::Field(n) | Target::Submit(n) | Target::Check(n)) => Some(n),
                _ => None,
            });
            if let Some(n) = node {
                let init = EventInit { bubbles: true, cancelable: true, client: Some((x as f32, (y - TOOLBAR_H) as f32)), ..Default::default() };
                let gen = self.gen;
                self.fire(n, "mousedown", init.clone());
                self.fire(n, "mouseup", init.clone());
                if let Some(Target::Check(c)) = t {
                    // A checkbox changes, then hears the click (which can undo it).
                    let was = self.shown.form.checked.clone();
                    self.toggle(c);
                    if self.fire(n, "click", init) {
                        self.fire(c, "input", EventInit { bubbles: true, ..Default::default() });
                        self.fire(c, "change", EventInit { bubbles: true, ..Default::default() });
                    } else {
                        self.shown.form.checked = was;
                    }
                    self.page_focus = Some(Target::Check(c));
                    self.apply_effects();
                    return;
                }
                let ok = self.fire(n, "click", init);
                self.apply_effects();
                if !ok || gen != self.gen {
                    return;
                }
            }
        }
        match t {
            Some(t) => {
                self.page_focus = None;
                self.shown.form.focus = None;
                self.activate(t);
            }
            None => {
                self.page_focus = None;
                self.shown.form.focus = None;
                if self.shown.rt.is_some() {
                    self.field_focus(None);
                }
            }
        }
    }

    // ---- drawing ----------------------------------------------------------

    pub fn tick(&mut self, now: u64) {
        self.now = now;
    }

    pub fn wants_caret(&self) -> bool {
        self.focus == Focus::Address || self.shown.form.focus.is_some()
    }

    pub fn blink(&mut self) {
        self.caret_on = !self.caret_on;
    }

    pub fn draw(&mut self, px: &mut [u32], w: u32, h: u32, pitch: u32) {
        let mut cv = Canvas::new(px, w, h, pitch);
        let (w, h) = (w as i32, h as i32);
        // The page.
        cv.clip = Rect::new(0, TOOLBAR_H, w, h - TOOLBAR_H);
        self.shown.form.caret_on = self.caret_on;
        paint_page(&mut cv, &mut self.cache, &self.shown.page, &self.shown.loaded.images, 0, TOOLBAR_H, self.shown.scroll, &self.shown.form);
        // Keyboard focus ring.
        if let Some(t) = self.page_focus {
            let accent = c(0x6D3BE8);
            for it in &self.shown.page.items {
                if let Item::Hit { x, y, w: hw, h: hh, target } = it {
                    if *target == t {
                        let yy = y + TOOLBAR_H as f32 - self.shown.scroll;
                        cv.round_outline(x - 2.0, yy - 2.0, hw + 4.0, hh + 4.0, 4.0, 2.0, accent);
                    }
                }
            }
        }
        // Scrollbar.
        if let Some((_, thumb)) = self.scrollbar() {
            let dark = self.shown.page.background.r as u32 + self.shown.page.background.g as u32 + self.shown.page.background.b as u32 > 384;
            let col = if dark { Color { r: 20, g: 24, b: 40, a: 110 } } else { Color { r: 230, g: 236, b: 255, a: 120 } };
            cv.round_rect(thumb.x as f32, thumb.y as f32, thumb.w as f32, thumb.h as f32, 3.0, col);
        }
        cv.clip = Rect::new(0, 0, w, h);
        self.draw_toolbar(&mut cv, w);
        self.draw_status(&mut cv, w, h);
    }

    fn draw_toolbar(&mut self, cv: &mut Canvas, w: i32) {
        cv.gradient(Rect::new(0, 0, w, TOOLBAR_H), c(BAR_TOP), c(BAR_BOTTOM), true, 0.0);
        cv.fill(Rect::new(0, TOOLBAR_H - 1, w, 1), c(EDGE));
        // A faint accent highlight along the top edge: the glass rim.
        cv.gradient(Rect::new(0, 0, w, 1), Color { r: 0x38, g: 0xE1, b: 0xF5, a: 70 }, Color { r: 0xB0, g: 0x6C, b: 0xFF, a: 70 }, false, 0.0);
        let can_back = self.index > 0;
        let can_fwd = self.index + 1 < self.history.len();
        let loading = self.loading.is_some();
        for (b, r) in self.button_rects() {
            let mode = self.script_mode();
            let enabled = match b {
                Button::Back => can_back,
                Button::Forward => can_fwd,
                Button::Scripts => mode.is_some(),
                _ => true,
            };
            if self.hover_button == Some(b) && enabled {
                cv.round_rect(r.x as f32, r.y as f32, r.w as f32, r.h as f32, 9.0, Color { r: 0x38, g: 0x4A, b: 0x80, a: 140 });
            }
            let col = if enabled { c(TEXT) } else { Color { r: 0x8C, g: 0x96, b: 0xB8, a: 110 } };
            let (cx, cy) = (r.x as f32 + 16.0, r.y as f32 + 16.0);
            match b {
                Button::Back => {
                    cv.stroke(&[(cx + 6.0, cy), (cx - 6.0, cy)], 2.0, col);
                    cv.stroke(&[(cx - 1.0, cy - 5.0), (cx - 6.0, cy), (cx - 1.0, cy + 5.0)], 2.0, col);
                }
                Button::Forward => {
                    cv.stroke(&[(cx - 6.0, cy), (cx + 6.0, cy)], 2.0, col);
                    cv.stroke(&[(cx + 1.0, cy - 5.0), (cx + 6.0, cy), (cx + 1.0, cy + 5.0)], 2.0, col);
                }
                Button::Reload if loading => {
                    cv.stroke(&[(cx - 5.0, cy - 5.0), (cx + 5.0, cy + 5.0)], 2.0, col);
                    cv.stroke(&[(cx + 5.0, cy - 5.0), (cx - 5.0, cy + 5.0)], 2.0, col);
                }
                Button::Reload => {
                    // A clockwise arc with an arrowhead at its end.
                    let (a0, a1, r) = (-0.9f32, 4.2f32, 6.5f32);
                    let pts: Vec<(f32, f32)> = (0..=24)
                        .map(|i| {
                            let a = a0 + i as f32 / 24.0 * (a1 - a0);
                            (cx + r * a.cos(), cy + r * a.sin())
                        })
                        .collect();
                    cv.stroke(&pts, 2.0, col);
                    let (ex, ey) = (cx + r * a0.cos(), cy + r * a0.sin());
                    // Tangent pointing backwards along the arc at its start.
                    let (tx, ty) = (a0.sin(), -a0.cos());
                    let (nx, ny) = (-ty, tx);
                    let head = [(ex - tx * 4.5 + nx * 3.5, ey - ty * 4.5 + ny * 3.5), (ex + tx * 0.8, ey + ty * 0.8), (ex - tx * 4.5 - nx * 3.5, ey - ty * 4.5 - ny * 3.5)];
                    cv.stroke(&head, 2.0, col);
                }
                Button::Scripts => {
                    // Braces: {} for scripts, cyan for this site's own, violet
                    // for all, crossed out when off.
                    let tint = match mode {
                        Some(Mode::Site) => c(ACCENT),
                        Some(Mode::All) => c(ACCENT2),
                        _ => col,
                    };
                    let bx = cx - 5.0;
                    let left = [(bx + 2.5, cy - 8.0), (bx + 0.7, cy - 7.0), (bx, cy - 5.0), (bx, cy - 2.0), (bx - 2.0, cy), (bx, cy + 2.0), (bx, cy + 5.0), (bx + 0.7, cy + 7.0), (bx + 2.5, cy + 8.0)];
                    let right: Vec<(f32, f32)> = left.iter().map(|&(x, y)| (2.0 * cx - x, y)).collect();
                    cv.stroke(&left, 1.8, tint);
                    cv.stroke(&right, 1.8, tint);
                    cv.fill(Rect::new(cx as i32 - 1, cy as i32 - 1, 2, 2), tint);
                    if mode == Some(Mode::Off) {
                        cv.stroke(&[(cx - 9.0, cy + 9.0), (cx + 9.0, cy - 9.0)], 2.0, c(DANGER));
                    }
                }
                Button::Shield => {
                    let s = [(cx, cy - 8.0), (cx + 7.0, cy - 5.0), (cx + 6.0, cy + 2.0), (cx, cy + 8.0), (cx - 6.0, cy + 2.0), (cx - 7.0, cy - 5.0), (cx, cy - 8.0)];
                    cv.stroke(&s, 1.8, c(SAFE));
                    cv.stroke(&[(cx - 3.0, cy), (cx - 0.5, cy + 2.5), (cx + 3.5, cy - 2.5)], 1.8, c(SAFE));
                }
            }
        }
        // The address field.
        let r = self.address_rect();
        let focused = self.focus == Focus::Address;
        cv.round_rect(r.x as f32, r.y as f32, r.w as f32, r.h as f32, 17.0, c(GLASS));
        let border = if focused { c(ACCENT) } else { c(EDGE) };
        cv.round_outline(r.x as f32, r.y as f32, r.w as f32, r.h as f32, 17.0, if focused { 1.6 } else { 1.0 }, border);
        // Padlock / page kind.
        let (lx, ly) = (r.x as f32 + 15.0, r.y as f32 + 9.0);
        let secure_state = match (&self.shown.kind, &self.shown.loaded.location) {
            _ if self.loading.is_some() => 3,
            (Kind::Interstitial(..), _) => 1,
            (_, Location::About(_)) => 2,
            (_, Location::Web(_)) if self.shown.loaded.secure => 0,
            _ => 1,
        };
        match secure_state {
            0 | 1 => {
                let col = if secure_state == 0 { c(SAFE) } else { c(DANGER) };
                cv.round_rect(lx, ly + 7.0, 12.0, 9.0, 2.0, col);
                // Shackle: closed when secure, open when not.
                let open: f32 = if secure_state == 1 { -4.0 } else { 0.0 };
                let pts: Vec<(f32, f32)> = (0..=12)
                    .map(|i| {
                        let a = std::f32::consts::PI * (1.0 + i as f32 / 12.0);
                        (lx + 6.0 + 4.0 * a.cos(), ly + 4.5 + 4.0 * a.sin())
                    })
                    .collect();
                let mut sh = vec![(lx + 2.0, ly + 7.5)];
                sh.extend(pts);
                sh.push((lx + 10.0, ly + 7.5 + open));
                cv.stroke(&sh, 1.7, col);
                cv.fill(Rect::new((lx + 5.5) as i32, (ly + 10.0) as i32, 1, 3), c(GLASS));
            }
            2 => {
                // A built-in page: æther's mark.
                cv.gradient(Rect::new(lx as i32, ly as i32 + 2, 13, 13), c(ACCENT), c(ACCENT2), true, 4.0);
            }
            _ => {
                // Spinner while loading.
                let t = self.now.wrapping_sub(self.loading.as_ref().map_or(0, |l| l.started)) as f32 / 120.0;
                let pts: Vec<(f32, f32)> = (0..=10)
                    .map(|i| {
                        let a = t + i as f32 / 10.0 * 4.2;
                        (lx + 6.0 + 6.0 * a.cos(), ly + 8.0 + 6.0 * a.sin())
                    })
                    .collect();
                cv.stroke(&pts, 2.0, c(ACCENT));
            }
        }
        // The address text.
        let face = Face::new(15.0, false, false, false);
        let tx = (r.x + 40) as f32;
        let base = r.y as f32 + 23.0;
        let old = cv.push_clip(Rect::new(r.x + 36, r.y, r.w - 50, r.h));
        if focused {
            let text = self.addr.clone();
            let caret_x = tx + face.width(&text[..self.addr_caret.min(text.len())]);
            // Scroll the text so the caret stays visible.
            let room = (r.w - 60) as f32;
            let off = if caret_x - tx > room { caret_x - tx - room } else { 0.0 };
            if self.addr_all && !text.is_empty() {
                cv.round_rect(tx - 2.0 - off, r.y as f32 + 7.0, face.width(&text) + 4.0, 20.0, 4.0, Color { r: 0x38, g: 0xE1, b: 0xF5, a: 70 });
            }
            if text.is_empty() {
                cv.text(&mut self.cache, tx + 6.0, base, "Enter an address", face, c(TEXT_DIM), 0.0);
            }
            cv.text(&mut self.cache, tx - off, base, &text, face, c(TEXT), 0.0);
            if self.caret_on {
                cv.fill(Rect::new((caret_x - off).round() as i32, r.y + 8, 2, 19), c(ACCENT));
            }
        } else {
            let text = self.address_text();
            if text.is_empty() {
                cv.text(&mut self.cache, tx, base, "Enter an address", face, c(TEXT_DIM), 0.0);
            } else {
                // Scheme dim, host bright, the rest dim.
                let (pre, host, rest) = split_for_display(&text);
                let mut x = tx;
                let dim = c(TEXT_DIM);
                let pre_col = if pre.starts_with("http:") { c(DANGER) } else { dim };
                x += cv.text(&mut self.cache, x, base, pre, face, pre_col, 0.0);
                x += cv.text(&mut self.cache, x, base, host, face, c(TEXT), 0.0);
                cv.text(&mut self.cache, x, base, rest, face, dim, 0.0);
            }
        }
        cv.clip = old;
        // Loading progress: a moving accent bar under the toolbar.
        if let Some(l) = &self.loading {
            let t = self.now.wrapping_sub(l.started) as f32;
            let frac = 1.0 - 1.0 / (1.0 + t / 900.0);
            let bw = (w as f32 * (0.08 + 0.85 * frac)) as i32;
            cv.gradient(Rect::new(0, TOOLBAR_H - 2, bw, 2), c(ACCENT), c(ACCENT2), false, 0.0);
        }
    }

    fn draw_status(&mut self, cv: &mut Canvas, w: i32, h: i32) {
        if self.status.is_empty() {
            return;
        }
        let face = Face::new(13.0, false, false, false);
        let text: String = self.status.chars().take(160).collect();
        let tw = face.width(&text).min((w - 40) as f32);
        let (bx, bh) = (6.0, 24.0);
        let by = (h - 30) as f32;
        let bw = tw + 22.0;
        // Out of the pointer's way.
        let bx = if self.mouse.1 > h - 40 && (self.mouse.0 as f32) < bx + bw { w as f32 - bw - 18.0 } else { bx };
        cv.round_rect(bx, by, bw, bh, 9.0, Color { r: 0x0E, g: 0x14, b: 0x28, a: 235 });
        cv.round_outline(bx, by, bw, bh, 9.0, 1.0, c(EDGE));
        let old = cv.push_clip(Rect::new(bx as i32 + 6, by as i32, bw as i32 - 12, bh as i32));
        cv.text(&mut self.cache, bx + 11.0, by + 16.5, &text, face, c(TEXT), 0.0);
        cv.clip = old;
    }

    /// The window title: the page's title, or the app's name.
    pub fn window_title(&self) -> String {
        let t = self.title.split_whitespace().collect::<Vec<_>>().join(" ");
        if t.is_empty() {
            "Browser".into()
        } else {
            t
        }
    }

    // ---- for tests -----------------------------------------------------------

    pub fn address(&self) -> String {
        if self.focus == Focus::Address {
            self.addr.clone()
        } else {
            self.address_text()
        }
    }

    /// Window coordinates of the first link whose href contains `needle`
    /// (scrolled into view), for tests.
    pub fn find_link(&mut self, needle: &str) -> Option<(i32, i32)> {
        let (x, y, w, h) = self.shown.page.items.iter().find_map(|it| match it {
            Item::Hit { x, y, w, h, target: Target::Link(i) } if self.shown.page.links.get(*i as usize).is_some_and(|l| l.contains(needle)) => Some((*x, *y, *w, *h)),
            _ => None,
        })?;
        let vh = self.view_h();
        if y < self.shown.scroll || y + h > self.shown.scroll + vh {
            self.scroll_to(y - vh / 3.0);
        }
        Some(((x + w / 2.0) as i32, (y + h / 2.0 - self.shown.scroll) as i32 + TOOLBAR_H))
    }

    /// Window coordinates of the first text run containing `needle`
    /// (scrolled into view), for tests.
    pub fn find_text(&mut self, needle: &str) -> Option<(i32, i32)> {
        let (x, y, w) = self.shown.page.items.iter().find_map(|it| match it {
            Item::Text { x, y, text, face, .. } if text.contains(needle) => Some((*x, *y, face.width(text))),
            _ => None,
        })?;
        let vh = self.view_h();
        if y - 20.0 < self.shown.scroll || y + 10.0 > self.shown.scroll + vh {
            self.scroll_to(y - vh / 3.0);
        }
        Some(((x + w / 2.0) as i32, (y - 5.0 - self.shown.scroll) as i32 + TOOLBAR_H))
    }

    pub fn toggle_scripts_for_test(&mut self) {
        self.toggle_scripts();
    }

    pub fn page_text(&self) -> String {
        self.shown.page.text()
    }

    pub fn scroll(&self) -> f32 {
        self.shown.scroll
    }

    pub fn status(&self) -> &str {
        &self.status
    }

    pub fn is_interstitial(&self) -> bool {
        matches!(self.shown.kind, Kind::Interstitial(..))
    }

    pub fn is_secure(&self) -> bool {
        self.shown.loaded.secure && matches!(self.shown.kind, Kind::Normal)
    }
}

/// The browser window: a strip of tabs and the selected tab's toolbar and
/// page. Each tab keeps its own history, scripts and http choices.
pub struct State {
    w: i32,
    h: i32,
    tabs: Vec<Tab>,
    active: usize,
    sync_loads: bool,
    fetcher: Fetcher<'static>,
    now: u64,
    /// What pages store (localStorage, sessionStorage, script cookies) lives
    /// here for the whole window, in memory. Tabs share the same origin-keyed
    /// jar, as they would in a browser.
    storage: Rc<RefCell<script::Storage>>,
    /// The selected tab is drawn here, then copied below the tab strip.
    scratch: Vec<u32>,
    /// A font cache for the tab strip (pages have their own).
    chrome_cache: Cache,
    mouse: (i32, i32),
    hover_tab: Option<usize>,
    hover_close: Option<usize>,
    hover_new: bool,
    quiet: bool,
}

impl State {
    pub fn new(w: i32, h: i32, sync_loads: bool) -> State {
        let tab_h = (h - TAB_H).max(1);
        let storage = Rc::new(RefCell::new(script::Storage::default()));
        let mut tab = Tab::new(w, tab_h, sync_loads);
        tab.storage = storage.clone();
        State {
            w,
            h,
            tabs: vec![tab],
            active: 0,
            sync_loads,
            fetcher: page::default_fetcher(),
            now: 0,
            storage,
            scratch: Vec::new(),
            chrome_cache: Cache::default(),
            mouse: (-1, -1),
            hover_tab: None,
            hover_close: None,
            hover_new: false,
            quiet: false,
        }
    }

    pub fn set_fetcher(&mut self, f: Fetcher<'static>) {
        self.fetcher = f;
        for t in &mut self.tabs {
            t.set_fetcher(f);
        }
    }

    fn tab(&self) -> &Tab {
        &self.tabs[self.active]
    }

    fn tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active]
    }

    fn tab_h(&self) -> i32 {
        (self.h - TAB_H).max(1)
    }

    // ---- tabs -------------------------------------------------------------

    pub fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    pub fn active_tab(&self) -> usize {
        self.active
    }

    pub fn tab_title(&self, i: usize) -> String {
        let Some(t) = self.tabs.get(i) else { return String::new() };
        let short: String = t.title.chars().take(512).collect();
        let title = short.split_whitespace().collect::<Vec<_>>().join(" ");
        if !title.is_empty() && title != "New page" {
            title
        } else if matches!(t.shown.loaded.location, Location::About(ref n) if n == "home") {
            "New Tab".into()
        } else {
            let a = t.address_text();
            if a.is_empty() { "New Tab".into() } else { a }
        }
    }

    pub fn new_tab(&mut self) -> usize {
        if self.tabs.len() >= MAX_TABS {
            self.tab_mut().status = "Too many tabs; close one first.".into();
            return self.active;
        }
        let mut t = Tab::new(self.w, self.tab_h(), self.sync_loads);
        t.set_fetcher(self.fetcher);
        t.storage = self.storage.clone();
        t.tick(self.now);
        t.focus_address();
        let at = (self.active + 1).min(self.tabs.len());
        self.tabs.insert(at, t);
        self.active = at;
        self.hover_tab = None;
        self.hover_close = None;
        at
    }

    /// Selects tab `i`; returns false if there is no such tab.
    pub fn select_tab(&mut self, i: usize) -> bool {
        if i >= self.tabs.len() {
            return false;
        }
        self.active = i;
        self.tabs[i].tick(self.now);
        self.tabs[i].start_pending_scripts();
        self.tabs[i].quiet = false;
        true
    }

    /// Closes tab `i`; true if the window should stay open. Closing the last
    /// tab closes the window, as in Chrome.
    pub fn close_tab(&mut self, i: usize) -> bool {
        if i >= self.tabs.len() {
            return true;
        }
        if self.tabs.len() == 1 {
            return false;
        }
        self.tabs[i].stop();
        self.tabs.remove(i);
        if i < self.active {
            self.active -= 1;
        } else if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        }
        self.tabs[self.active].tick(self.now);
        self.hover_tab = None;
        self.hover_close = None;
        true
    }

    fn next_tab(&mut self, back: bool) {
        let n = self.tabs.len();
        if n < 2 {
            return;
        }
        let i = if back { (self.active + n - 1) % n } else { (self.active + 1) % n };
        self.select_tab(i);
    }

    fn tab_rects(&self) -> Vec<(usize, Rect)> {
        let n = self.tabs.len();
        if n == 0 {
            return Vec::new();
        }
        let room = (self.w - 8 - 40).max(40);
        let gap = 2;
        let mut w = ((room - gap * (n as i32 - 1)) / n as i32).clamp(24, 220);
        if w * n as i32 + gap * (n as i32 - 1) > room {
            w = ((room - gap * (n as i32 - 1)) / n as i32).max(16);
        }
        let mut x = 8;
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            out.push((i, Rect::new(x, 4, w, TAB_H - 4)));
            x += w + gap;
        }
        out
    }

    fn close_rect(&self, tab: Rect) -> Rect {
        Rect::new(tab.x + tab.w - 24, tab.y + 7, 18, 20)
    }

    fn new_tab_rect(&self) -> Rect {
        let rects = self.tab_rects();
        let x = rects.last().map_or(8, |(_, r)| r.x + r.w + 6);
        Rect::new(x, 4, 28, TAB_H - 4)
    }

    fn update_tab_hover(&mut self, x: i32, y: i32) {
        let mut tab = None;
        let mut close = None;
        for (i, r) in self.tab_rects() {
            if r.contains(x, y) {
                tab = Some(i);
                if r.w >= 88 && self.close_rect(r).contains(x, y) {
                    close = Some(i);
                }
            }
        }
        self.hover_tab = tab;
        self.hover_close = close;
        self.hover_new = self.new_tab_rect().contains(x, y);
    }

    fn tab_click(&mut self, x: i32, y: i32) -> bool {
        if self.new_tab_rect().contains(x, y) {
            self.new_tab();
            return true;
        }
        for (i, r) in self.tab_rects() {
            if r.contains(x, y) {
                if r.w >= 88 && self.close_rect(r).contains(x, y) {
                    return self.close_tab(i);
                }
                self.select_tab(i);
                return true;
            }
        }
        true
    }

    // ---- navigation ------------------------------------------------------

    pub fn open_typed(&mut self, text: &str) {
        self.tab_mut().open_typed(text);
    }

    pub fn back(&mut self) {
        self.tab_mut().back();
    }

    pub fn forward(&mut self) {
        self.tab_mut().forward();
    }

    pub fn reload(&mut self) {
        self.tab_mut().reload();
    }

    pub fn stop(&mut self) {
        self.tab_mut().stop();
    }

    pub fn is_loading(&self) -> bool {
        self.tab().is_loading()
    }

    // ---- scripts, polling and time --------------------------------------

    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        for i in 0..self.tabs.len() {
            if i == self.active {
                changed |= self.tabs[i].poll();
            } else {
                changed |= self.tabs[i].poll_background();
            }
        }
        changed
    }

    pub fn script_wake(&self) -> Option<u32> {
        self.tab().script_wake()
    }

    pub fn script_errors(&self) -> Vec<String> {
        self.tab().script_errors()
    }

    pub fn toggle_scripts_for_test(&mut self) {
        self.tab_mut().toggle_scripts_for_test();
    }

    pub fn tick(&mut self, now: u64) {
        self.now = now;
        for t in &mut self.tabs {
            t.tick(now);
        }
    }

    pub fn wants_caret(&self) -> bool {
        self.tab().wants_caret()
    }

    pub fn blink(&mut self) {
        self.tab_mut().blink();
    }

    // ---- input -----------------------------------------------------------

    pub fn handle(&mut self, ev: Event) -> bool {
        self.quiet = false;
        match ev {
            Event::Close => return false,
            Event::Resize { width, height } => {
                self.resize(width as i32, height as i32);
                return true;
            }
            Event::Key { key, mods } => {
                let ctrl = mods & abi::MOD_CTRL != 0;
                if ctrl && (key == 'w' as u32 || key == 'W' as u32 || key == 23) {
                    // Closing the last tab closes the window, as in Chrome.
                    return self.close_tab(self.active);
                }
                if self.tab_shortcut(key, mods) {
                    return true;
                }
                let keep = self.tab_mut().handle(Event::Key { key, mods });
                self.quiet = self.tab().quiet;
                return keep;
            }
            Event::MouseMove(m) => {
                self.mouse = (m.x, m.y);
                if m.y < TAB_H {
                    let before = (self.hover_tab, self.hover_close, self.hover_new);
                    self.update_tab_hover(m.x, m.y);
                    self.quiet = before == (self.hover_tab, self.hover_close, self.hover_new);
                    return true;
                }
                let mut m = m;
                m.y -= TAB_H;
                let keep = self.tab_mut().handle(Event::MouseMove(m));
                self.quiet = self.tab().quiet;
                return keep;
            }
            Event::MouseDown(m) => {
                self.mouse = (m.x, m.y);
                if m.y < TAB_H {
                    self.update_tab_hover(m.x, m.y);
                    if m.buttons & 1 != 0 {
                        return self.tab_click(m.x, m.y);
                    }
                    if m.buttons & 4 != 0 {
                        // Middle-click closes the tab under the pointer.
                        if let Some((i, _)) = self.tab_rects().into_iter().find(|(_, r)| r.contains(m.x, m.y)) {
                            return self.close_tab(i);
                        }
                    }
                    return true;
                }
                let mut m = m;
                m.y -= TAB_H;
                let keep = self.tab_mut().handle(Event::MouseDown(m));
                self.quiet = self.tab().quiet;
                return keep;
            }
            Event::MouseUp(m) => {
                self.mouse = (m.x, m.y);
                let mut m = m;
                m.y -= TAB_H;
                let keep = self.tab_mut().handle(Event::MouseUp(m));
                self.quiet = self.tab().quiet;
                return keep;
            }
            Event::Scroll { dx, dy, mods } => {
                if self.mouse.1 >= 0 && self.mouse.1 < TAB_H {
                    return true;
                }
                let keep = self.tab_mut().handle(Event::Scroll { dx, dy, mods });
                self.quiet = self.tab().quiet;
                return keep;
            }
            Event::Focus | Event::Unfocus => {
                let keep = self.tab_mut().handle(ev);
                self.quiet = self.tab().quiet;
                return keep;
            }
        }
    }

    /// Window-level shortcuts (tabs) run before the page's own keys.
    fn tab_shortcut(&mut self, key: u32, mods: u32) -> bool {
        let ctrl = mods & abi::MOD_CTRL != 0;
        let shift = mods & abi::MOD_SHIFT != 0;
        if !ctrl {
            return false;
        }
        if key == 't' as u32 || key == 'T' as u32 || key == 20 {
            self.new_tab();
            return true;
        }
        if key == '\t' as u32 {
            self.next_tab(shift);
            return true;
        }
        if (b'1' as u32..=b'9' as u32).contains(&key) {
            let n = self.tabs.len();
            let i = if key == b'9' as u32 { n - 1 } else { (key - b'1' as u32) as usize };
            self.select_tab(i);
            return true;
        }
        false
    }

    pub fn resize(&mut self, w: i32, h: i32) {
        if w == self.w && h == self.h {
            return;
        }
        self.w = w;
        self.h = h;
        let th = self.tab_h();
        for t in &mut self.tabs {
            t.resize(w, th);
        }
    }

    // ---- drawing ---------------------------------------------------------

    pub fn draw(&mut self, px: &mut [u32], w: u32, h: u32, pitch: u32) {
        let (wi, hi) = (w as i32, h as i32);
        let th = (hi - TAB_H).max(0);
        if th > 0 {
            let need = w as usize * th as usize;
            if self.scratch.len() < need {
                self.scratch.resize(need, 0);
            }
            {
                let tab = &mut self.tabs[self.active];
                tab.draw(&mut self.scratch[..need], w, th as u32, w);
            }
            for row in 0..th as usize {
                let src = &self.scratch[row * w as usize..(row + 1) * w as usize];
                let start = (row + TAB_H as usize) * pitch as usize;
                let dst = &mut px[start..start + w as usize];
                dst.copy_from_slice(src);
            }
        }
        let mut cv = Canvas::new(px, w, h, pitch);
        cv.clip = Rect::new(0, 0, wi, TAB_H.min(hi).max(0));
        self.draw_tabs(&mut cv, wi);
    }

    fn draw_tabs(&mut self, cv: &mut Canvas, w: i32) {
        cv.fill(Rect::new(0, 0, w, TAB_H), c(0x0B1020));
        cv.fill(Rect::new(0, TAB_H - 1, w, 1), c(EDGE));
        let rects = self.tab_rects();
        for (i, r) in &rects {
            let active = *i == self.active;
            let hot = self.hover_tab == Some(*i);
            let bg = if active { c(0x1A2344) } else if hot { c(0x151B33) } else { c(0x10162A) };
            let old = cv.push_clip(Rect::new(0, 0, w, TAB_H));
            cv.round_rect(r.x as f32, r.y as f32, r.w as f32, (r.h + 12) as f32, 10.0, bg);
            cv.clip = old;
            if active {
                cv.fill(Rect::new(r.x + 10, r.y, r.w - 20, 2), c(ACCENT));
            }
            // Favicon: a small page mark, a lock for https, or a spinner.
            let (ix, iy) = (r.x + 10, r.y + 11);
            let loading = self.tabs[*i].loading.is_some();
            if loading {
                let t = self.now.wrapping_sub(self.tabs[*i].loading.as_ref().map_or(0, |l| l.started)) as f32 / 140.0;
                let pts: Vec<(f32, f32)> = (0..=10)
                    .map(|k| {
                        let a = t + k as f32 / 10.0 * 4.2;
                        (ix as f32 + 7.0 + 5.0 * a.cos(), iy as f32 + 7.0 + 5.0 * a.sin())
                    })
                    .collect();
                cv.stroke(&pts, 1.8, c(ACCENT));
            } else {
                let secure = self.tabs[*i].is_secure();
                let col = if secure { c(SAFE) } else { c(0x4A5578) };
                cv.round_rect(ix as f32, iy as f32, 14.0, 14.0, 4.0, col);
                if secure {
                    // A tiny closed padlock.
                    cv.fill(Rect::new(ix + 4, iy + 7, 6, 5), c(0x0B1020));
                    cv.stroke(&[(ix as f32 + 5.0, iy as f32 + 7.0), (ix as f32 + 5.0, iy as f32 + 4.5), (ix as f32 + 7.0, iy as f32 + 3.0), (ix as f32 + 9.0, iy as f32 + 4.5), (ix as f32 + 9.0, iy as f32 + 7.0)], 1.2, c(0x0B1020));
                } else {
                    let f = Face::new(9.0, true, false, false);
                    let ch = self.tab_title(*i).chars().next().unwrap_or('?');
                    let s = ch.to_string();
                    let tw = f.width(&s);
                    cv.text(&mut self.chrome_cache, ix as f32 + (14.0 - tw) / 2.0, iy as f32 + 11.0, &s, f, c(TEXT), 0.0);
                }
            }
            // Title.
            let face = Face::new(12.5, false, false, false);
            let title = ellipsize_text(&self.tab_title(*i), face, (r.w - 52).max(0) as f32);
            cv.text(&mut self.chrome_cache, (r.x + 30) as f32, (r.y + 22) as f32, &title, face, if active { c(TEXT) } else { c(TEXT_DIM) }, 0.0);
            // Close button on the active or hovered tab.
            if r.w >= 88 && (active || hot) {
                let cr = self.close_rect(*r);
                if self.hover_close == Some(*i) {
                    cv.round_rect(cr.x as f32, cr.y as f32, cr.w as f32, cr.h as f32, 7.0, c(0x2A3356));
                }
                let (cx, cy) = (cr.x as f32 + 9.0, cr.y as f32 + 10.0);
                cv.stroke(&[(cx - 3.5, cy - 3.5), (cx + 3.5, cy + 3.5)], 1.5, c(TEXT_DIM));
                cv.stroke(&[(cx + 3.5, cy - 3.5), (cx - 3.5, cy + 3.5)], 1.5, c(TEXT_DIM));
            }
        }
        // New tab button.
        let nr = self.new_tab_rect();
        if self.hover_new {
            cv.round_rect(nr.x as f32, nr.y as f32, nr.w as f32, nr.h as f32, 8.0, c(0x1A2344));
        }
        let (cx, cy) = (nr.x as f32 + nr.w as f32 / 2.0, nr.y as f32 + nr.h as f32 / 2.0);
        cv.stroke(&[(cx - 5.0, cy), (cx + 5.0, cy)], 1.8, c(TEXT_DIM));
        cv.stroke(&[(cx, cy - 5.0), (cx, cy + 5.0)], 1.8, c(TEXT_DIM));
    }

    // ---- for tests -------------------------------------------------------

    pub fn address(&self) -> String {
        self.tab().address()
    }

    pub fn find_link(&mut self, needle: &str) -> Option<(i32, i32)> {
        self.tab_mut().find_link(needle).map(|(x, y)| (x, y + TAB_H))
    }

    pub fn find_text(&mut self, needle: &str) -> Option<(i32, i32)> {
        self.tab_mut().find_text(needle).map(|(x, y)| (x, y + TAB_H))
    }

    pub fn page_text(&self) -> String {
        self.tab().page_text()
    }

    pub fn scroll(&self) -> f32 {
        self.tab().scroll()
    }

    pub fn status(&self) -> &str {
        self.tab().status()
    }

    pub fn is_interstitial(&self) -> bool {
        self.tab().is_interstitial()
    }

    pub fn is_secure(&self) -> bool {
        self.tab().is_secure()
    }

    /// How many pictures the current page decoded (for tests).
    pub fn image_count(&self) -> usize {
        self.tab().shown.loaded.images.len()
    }

    pub fn window_title(&self) -> String {
        self.tab().window_title().chars().take(512).collect()
    }
}

/// Cuts `s` to fit in `max` pixels, with an ellipsis when it doesn't.
fn ellipsize_text(s: &str, face: Face, max: f32) -> String {
    if face.width(s) <= max {
        return s.to_string();
    }
    let dots = "...";
    let mut out = String::new();
    for ch in s.chars() {
        let mut next = out.clone();
        next.push(ch);
        if face.width(&next) + face.width(dots) > max {
            break;
        }
        out = next;
    }
    out.push_str(dots);
    out
}


/// The scripts button's description of a mode.
fn scripts_label(m: Mode, errors: usize) -> &'static str {
    match (m, errors > 0) {
        (Mode::Site, false) => "Scripts: on, this site's own only. Click to allow other sites' scripts too.",
        (Mode::Site, true) => "Scripts: on, this site's own only (some failed). Click to allow other sites' scripts too.",
        (Mode::All, false) => "Scripts: on, including other sites'. Click to turn scripts off.",
        (Mode::All, true) => "Scripts: on, including other sites' (some failed). Click to turn scripts off.",
        (Mode::Off, _) => "Scripts: off for this site. Click to turn them on.",
    }
}

/// What a key looks like to a page's scripts.
fn key_init(key: u32, mods: u32) -> EventInit {
    let (name, code): (String, u32) = match key {
        8 => ("Backspace".into(), 8),
        9 => ("Tab".into(), 9),
        10 | 13 => ("Enter".into(), 13),
        27 => ("Escape".into(), 27),
        abi::KEY_LEFT => ("ArrowLeft".into(), 37),
        abi::KEY_UP => ("ArrowUp".into(), 38),
        abi::KEY_RIGHT => ("ArrowRight".into(), 39),
        abi::KEY_DOWN => ("ArrowDown".into(), 40),
        abi::KEY_PGUP => ("PageUp".into(), 33),
        abi::KEY_PGDN => ("PageDown".into(), 34),
        abi::KEY_HOME => ("Home".into(), 36),
        abi::KEY_END => ("End".into(), 35),
        abi::KEY_DELETE => ("Delete".into(), 46),
        k => match char::from_u32(k) {
            Some(ch) if !ch.is_control() => (ch.to_string(), ch.to_ascii_uppercase() as u32),
            _ => ("Unidentified".into(), 0),
        },
    };
    EventInit {
        bubbles: true,
        cancelable: true,
        key: Some(name),
        key_code: code,
        shift: mods & abi::MOD_SHIFT != 0,
        ctrl: mods & abi::MOD_CTRL != 0,
        alt: mods & abi::MOD_ALT != 0,
        ..Default::default()
    }
}

/// Capitalises the first letter and ends with a full stop.
fn sentence(s: &str) -> String {
    let mut c = s.chars();
    let mut o: String = match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => return String::new(),
    };
    if !o.ends_with(['.', '!', '?', ')']) {
        o.push('.');
    }
    o
}

fn split_fragment(u: Url) -> (Url, Option<String>) {
    match u.as_str().split_once('#') {
        Some((a, f)) => (Url::parse(a).unwrap_or(u.clone()), Some(f.to_string())),
        None => (u, None),
    }
}

fn strip_fragment(s: &str) -> &str {
    s.split('#').next().unwrap_or(s)
}

fn split_for_display(s: &str) -> (&str, &str, &str) {
    if let Some(i) = s.find("://") {
        let rest = &s[i + 3..];
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        (&s[..i + 3], &rest[..end], &rest[end..])
    } else {
        ("", s, "")
    }
}

/// Runs the browser in a window until it's closed.
pub fn run(mut backend: Box<dyn Backend + Send>, start: Option<String>, fetcher: Fetcher<'static>) -> Result<(), aether_host::Error> {
    let info = backend.display_info();
    let want_w = 1100.min(info.max_client_w).max(320);
    let want_h = 760.min(info.max_client_h).max(240);
    let (win, w, h): (WinHandle, u32, u32) = backend.create_window(want_w, want_h, "Browser")?;
    // The page reflows at any size (Event::Resize below), so the user may zoom and resize it.
    let _ = backend.set_resizable(win, true, 480, 320);
    let sync = backend.name() == "headless";
    let mut st = State::new(w as i32, h as i32, sync);
    st.set_fetcher(fetcher);
    if let Some(u) = start {
        st.open_typed(&u);
    }
    let mut title = String::new();
    let mut last_blink = backend.uptime_ms();
    let mut dirty = true;
    loop {
        let now = backend.uptime_ms();
        st.tick(now);
        if st.poll() {
            dirty = true;
        }
        if dirty {
            if let Some(fb) = backend.framebuffer(win) {
                let (fw, fh, pitch) = (fb.width, fb.height, fb.pitch);
                st.resize(fw as i32, fh as i32);
                st.draw(fb.pixels, fw, fh, pitch);
            }
            backend.present(win)?;
            let t = st.window_title();
            if t != title {
                let mut buf = [0u8; abi::WIN_TITLE_MAX];
                let _ = backend.set_title(win, aether_host::aether::ascii_title(&t, &mut buf));
                title = t;
            }
            dirty = false;
        }
        let mut timeout = if st.is_loading() {
            Some(40)
        } else if st.wants_caret() {
            Some(530u32.saturating_sub((backend.uptime_ms() - last_blink) as u32).max(1))
        } else {
            None
        };
        if let Some(w) = st.script_wake() {
            timeout = Some(timeout.map_or(w, |t| t.min(w)));
        }
        match backend.wait_event(timeout)? {
            Some(ev) => {
                if !st.handle(ev) {
                    break;
                }
                dirty |= !st.quiet;
            }
            None => {
                let now = backend.uptime_ms();
                if st.wants_caret() && now - last_blink >= 500 {
                    st.blink();
                    last_blink = now;
                    dirty = true;
                }
            }
        }
    }
    Ok(())
}
