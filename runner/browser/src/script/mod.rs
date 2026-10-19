//! script - "æther script", the browser's own JavaScript engine, and the
//! runtime that connects it to a page.
//!
//! The language is JavaScript as pages write it (ES2020-ish: classes,
//! arrow functions, destructuring, promises, async/await), with what makes
//! scripts hard to control taken out: no code from strings (`eval`,
//! `new Function`, string timers), no generators or modules, and every
//! task runs on a step budget, a recursion limit and a memory cap. What a
//! page's scripts may reach is decided here too: their own site's servers
//! and nothing else, no pop-ups, no storage that outlives the browser.
//! See docs/BROWSER.md ("Scripts").

pub mod ast;
pub mod builtins;
pub mod dom;
pub mod interp;
pub mod lexer;
pub mod parser;
pub mod regex;
pub mod value;
pub mod window;

use std::cell::RefCell;
use std::rc::Rc;

use crate::dom::{Document, NodeId};
use crate::layout::Page;
use crate::net::Url;
use crate::page::Fetcher;
use crate::paint::FormState;

pub use dom::{EventInit, NavReq, Storage};
use interp::{Ctrl, Interp};
use value::Value;

/// Which scripts a site may run (chosen per site with the toolbar button).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mode {
    /// No scripts at all.
    Off,
    /// The site's own scripts (its domain and subdomains): the default.
    Site,
    /// Also scripts the page loads from other sites.
    All,
}

impl Mode {
    pub fn runs(self) -> bool {
        self != Mode::Off
    }
}

/// The user's choices of Mode, per site (for this run of the browser only).
#[derive(Clone, Debug, Default)]
pub struct Policy {
    pub sites: std::collections::HashMap<String, Mode>,
}

impl Policy {
    /// Scripts run by default on https sites (their own scripts only), and
    /// not on plain-http pages, which anyone on the way could have changed.
    pub fn mode_for(&self, u: &Url) -> Mode {
        if let Some(m) = self.sites.get(&dom::site_of(u.host())) {
            return *m;
        }
        if u.scheme() == "https" {
            Mode::Site
        } else {
            Mode::Off
        }
    }

    pub fn set(&mut self, u: &Url, m: Mode) {
        self.sites.insert(dom::site_of(u.host()), m);
    }
}

/// A script of the page as loaded: its element and its text.
#[derive(Clone, Debug)]
pub struct PageScript {
    pub node: NodeId,
    pub source: Source,
}

#[derive(Clone, Debug)]
pub enum Source {
    Inline(String),
    Fetched { url: String, text: String },
    /// Not loaded (another site, too big, a network error): why.
    Refused(String),
}

/// Steps the page's own scripts may take together while it loads.
pub const LOAD_STEPS: u64 = 120_000_000;
/// Steps timers and fetch callbacks may take in one turn of the window.
pub const TICK_STEPS: u64 = 40_000_000;

/// What a page's scripts asked the window to do, collected after each task.
#[derive(Default, Debug)]
pub struct Effects {
    /// The document changed: lay it out again.
    pub relayout: bool,
    pub repaint: bool,
    pub nav: Option<NavReq>,
    pub url_changed: Option<(Url, bool)>,
    pub submit: Option<NodeId>,
    pub focus: Option<Option<NodeId>>,
    pub scroll: Option<(f32, f32)>,
    pub alerts: Vec<String>,
}

pub struct Setup {
    pub url: Url,
    pub base: Option<Url>,
    pub third_party: bool,
    pub fetcher: Option<Fetcher<'static>>,
    pub storage: Rc<RefCell<Storage>>,
    pub viewport: (f32, f32),
}

/// One page's scripts: the interpreter and everything it holds.
pub struct Runtime {
    it: Interp,
    gc_at: usize,
}

impl Runtime {
    pub fn new(s: Setup) -> Runtime {
        let mut host = dom::Host::default();
        host.url = Some(s.url);
        host.base = s.base;
        host.third_party = s.third_party;
        host.fetcher = s.fetcher;
        host.storage = s.storage;
        host.viewport = s.viewport;
        let it = Interp::new(host);
        let gc_at = it.heap.live * 2 + 20_000;
        Runtime { it, gc_at }
    }

    /// Lends the page's document and form state to the scripts for `f`.
    fn lend<T>(&mut self, doc: &mut Document, form: &mut FormState, f: impl FnOnce(&mut Interp) -> T) -> T {
        std::mem::swap(doc, &mut self.it.host.doc);
        std::mem::swap(form, &mut self.it.host.form);
        let r = f(&mut self.it);
        std::mem::swap(doc, &mut self.it.host.doc);
        std::mem::swap(form, &mut self.it.host.form);
        r
    }

    /// Runs one task, then its microtasks; uncaught errors go to the console.
    fn task(it: &mut Interp, f: impl FnOnce(&mut Interp) -> Result<(), Ctrl>) -> u64 {
        let start = it.steps;
        it.start_task();
        let r = f(it).and_then(|_| drain_microtasks(it));
        match r {
            Ok(()) => {}
            Err(Ctrl::Abort(m)) => {
                it.host.error(format!("A script was stopped: {m}."));
                it.microtasks.clear();
            }
            Err(e) => {
                dom::report(it, e);
                let _ = drain_microtasks(it);
            }
        }
        for p in std::mem::take(&mut it.unhandled) {
            if let value::Kind::Promise(pr) = &it.heap.get(p).kind {
                if let value::PState::Rejected(e) = &pr.state {
                    if !pr.handled {
                        let e = e.clone();
                        let m = dom::describe_error(it, &e);
                        it.host.error(format!("Uncaught (in promise) {m}"));
                    }
                }
            }
        }
        it.steps - start
    }

    fn maybe_gc(&mut self) {
        if self.it.heap.live > self.gc_at {
            self.it.gc();
            self.gc_at = (self.it.heap.live * 2).max(50_000);
        }
    }

    /// Runs the page's scripts in order, then DOMContentLoaded and load.
    pub fn run_page(&mut self, doc: &mut Document, form: &mut FormState, scripts: &[PageScript]) {
        self.lend(doc, form, |it| {
            // Every script element present now has had its turn, run or not.
            let all: Vec<NodeId> = it.host.doc.find_all("script");
            it.host.started.extend(all);
            let mut used = 0u64;
            let mut skipped = 0;
            for sc in scripts {
                if used > LOAD_STEPS {
                    skipped += 1;
                    continue;
                }
                it.host.write_point = Some(sc.node);
                used += match &sc.source {
                    Source::Inline(t) => Self::task(it, |it| dom::run_source(it, t, Some(sc.node))),
                    Source::Fetched { text, .. } => Self::task(it, |it| {
                        dom::run_source(it, text, Some(sc.node))?;
                        dom::fire_simple(it, sc.node, "load").map(|_| ())
                    }),
                    Source::Refused(why) => {
                        it.host.log(format!("script not run: {why}"));
                        Self::task(it, |it| dom::fire_simple(it, sc.node, "error").map(|_| ()))
                    }
                };
            }
            if skipped > 0 {
                it.host.error(format!("{skipped} script(s) skipped: the page's scripts ran too long"));
            }
            it.host.write_point = None;
            it.host.ready = "interactive";
            Self::task(it, |it| {
                let ev = dom::new_event(it, "readystatechange", &EventInit::default());
                let d = dom::wrap(it, Document::ROOT);
                dom::dispatch(it, &d, ev)?;
                let ev = dom::new_event(it, "DOMContentLoaded", &EventInit { bubbles: true, ..Default::default() });
                dom::dispatch(it, &d, ev).map(|_| ())
            });
            it.host.ready = "complete";
            Self::task(it, |it| {
                let ev = dom::new_event(it, "readystatechange", &EventInit::default());
                let d = dom::wrap(it, Document::ROOT);
                dom::dispatch(it, &d, ev)?;
                let w = Value::Obj(it.global);
                let ev = dom::new_event(it, "load", &EventInit::default());
                dom::dispatch(it, &w, ev)?;
                let ev = dom::new_event(it, "pageshow", &EventInit::default());
                dom::dispatch(it, &w, ev).map(|_| ())
            });
            // With scripts running, <noscript> content is not shown.
            if !scripts.is_empty() {
                for n in it.host.doc.find_all("noscript") {
                    dom::set_attr(it, n, "hidden", "");
                }
            }
        });
        self.maybe_gc();
    }

    /// Fires an event the user caused at node `n`; false if the page
    /// cancelled it (the browser then skips its default action).
    pub fn fire(&mut self, doc: &mut Document, form: &mut FormState, n: NodeId, ty: &str, init: EventInit) -> bool {
        let mut ok = true;
        self.lend(doc, form, |it| {
            if n >= it.host.doc.nodes.len() {
                return;
            }
            Self::task(it, |it| {
                let ev = dom::new_event(it, ty, &init);
                let t = dom::wrap(it, n);
                ok = dom::dispatch(it, &t, ev)?;
                Ok(())
            });
        });
        self.maybe_gc();
        ok
    }

    /// An event at the window (resize, scroll, hashchange...).
    pub fn fire_window(&mut self, doc: &mut Document, form: &mut FormState, ty: &str) {
        self.lend(doc, form, |it| {
            Self::task(it, |it| {
                let ev = dom::new_event(it, ty, &EventInit::default());
                let w = Value::Obj(it.global);
                dom::dispatch(it, &w, ev).map(|_| ())
            });
        });
    }

    /// Runs timers that are due and hands over finished requests. True if
    /// anything ran.
    pub fn tick(&mut self, doc: &mut Document, form: &mut FormState) -> bool {
        let mut ran = false;
        self.lend(doc, form, |it| {
            let mut budget = TICK_STEPS;
            let done = dom::finished_fetches(&mut it.host, 0);
            for (f, r) in done {
                ran = true;
                let used = Self::task(it, |it| dom::deliver(it, f, r));
                budget = budget.saturating_sub(used);
            }
            let now = it.host.now();
            let mut due: Vec<(f64, u32)> = it.host.timers.iter().filter(|t| t.due <= now).map(|t| (t.due, t.id)).collect();
            due.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
            for (_, id) in due.into_iter().take(200) {
                if budget == 0 {
                    break;
                }
                let Some(i) = it.host.timers.iter().position(|t| t.id == id) else { continue };
                ran = true;
                let used = Self::task(it, |it| dom::fire_timer(it, i));
                budget = budget.saturating_sub(used);
            }
        });
        if ran {
            self.maybe_gc();
        }
        ran
    }

    /// Milliseconds until a timer is due (0: now), or None when nothing waits.
    pub fn next_due(&self) -> Option<f64> {
        let h = &self.it.host;
        let t = h.timers.iter().map(|t| t.due).fold(f64::INFINITY, f64::min);
        let mut wait = if t.is_finite() { Some((t - h.now()).max(0.0)) } else { None };
        if !h.fetches.is_empty() {
            wait = Some(wait.map_or(30.0, |w: f64| w.min(30.0)));
        }
        wait
    }

    /// Runs the page's timers and requests ahead in time (up to `ms` of
    /// timer time), for headless runs that can't wait.
    pub fn settle(&mut self, doc: &mut Document, form: &mut FormState, ms: f64) {
        let end = self.it.host.now() + ms;
        for _ in 0..1000 {
            if !self.it.host.fetches.is_empty() {
                let done = dom::finished_fetches(&mut self.it.host, 20_000);
                let it_done = done;
                self.lend(doc, form, |it| {
                    for (f, r) in it_done {
                        Self::task(it, |it| dom::deliver(it, f, r));
                    }
                });
                continue;
            }
            let next = self.it.host.timers.iter().map(|t| t.due).fold(f64::INFINITY, f64::min);
            if !next.is_finite() || next > end {
                break;
            }
            let now = self.it.host.now();
            if next > now {
                self.it.host.warp += next - now;
            }
            self.tick(doc, form);
        }
    }

    /// Runs page code the user started (a javascript: link).
    pub fn run_script(&mut self, doc: &mut Document, form: &mut FormState, src: &str) {
        self.lend(doc, form, |it| {
            Self::task(it, |it| dom::run_source(it, src, None));
        });
        self.maybe_gc();
    }

    /// The page's address changed without a load (a #fragment link).
    pub fn set_url(&mut self, u: Url) {
        self.it.host.url = Some(u);
    }

    /// Tells the scripts where things are after a layout.
    pub fn set_layout(&mut self, page: &Page, scroll: f32, viewport: (f32, f32)) {
        let h = &mut self.it.host;
        h.rects = page.node_rects();
        h.page_h = page.height;
        h.scroll = (0.0, scroll);
        h.viewport = viewport;
    }

    pub fn set_scroll(&mut self, y: f32) {
        self.it.host.scroll = (0.0, y);
    }

    pub fn set_focus(&mut self, n: Option<NodeId>) {
        self.it.host.focused = n;
    }

    pub fn take_effects(&mut self) -> Effects {
        let h = &mut self.it.host;
        Effects {
            relayout: std::mem::take(&mut h.dirty),
            repaint: std::mem::take(&mut h.repaint),
            nav: h.nav.take(),
            url_changed: h.url_changed.take(),
            submit: h.submit.take(),
            focus: h.focus_req.take(),
            scroll: h.scroll_req.take(),
            alerts: std::mem::take(&mut h.alerts),
        }
    }

    /// Errors and messages so far (newest last).
    pub fn errors(&self) -> &[String] {
        &self.it.host.errors
    }

    pub fn console(&self) -> &[String] {
        &self.it.host.console
    }

    /// Runs `src` and returns String(r) (r: a global), for tests.
    #[cfg(test)]
    pub fn eval_r(&mut self, doc: &mut Document, form: &mut FormState, src: &str) -> String {
        self.lend(doc, form, |it| {
            Self::task(it, |it| dom::run_source(it, src, None));
            let g = Value::Obj(it.global);
            let v = it.get(&g, "r").unwrap_or(Value::Undef);
            it.to_str(&v).map(|s| s.to_string()).unwrap_or_default()
        })
    }

    /// Requests still in flight.
    pub fn busy(&self) -> bool {
        !self.it.host.fetches.is_empty()
    }
}

fn drain_microtasks(it: &mut Interp) -> Result<(), Ctrl> {
    let mut n = 0u64;
    while let Some((f, args)) = it.microtasks.pop_front() {
        n += 1;
        if n > 10_000_000 {
            return Err(Ctrl::Abort("too many promise callbacks".into()));
        }
        match it.call(&f, Value::Undef, args) {
            Ok(_) => {}
            Err(Ctrl::Abort(m)) => return Err(Ctrl::Abort(m)),
            Err(e) => dom::report(it, e),
        }
    }
    Ok(())
}

/// Runs `f` on a thread with the stack scripts get in the browser (tests
/// run on small default stacks).
#[cfg(test)]
pub fn big_stack(f: fn()) {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap();
}

#[cfg(test)]
mod tests;
