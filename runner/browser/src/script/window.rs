//! script/window.rs - the window object: timers, location and history,
//! navigator, console, storage, fetch, and the parts of the platform written
//! in script itself (prelude.js).
//!
//! The rules that make scripts safe to run live here as much as anywhere:
//! a script may only talk to its own origin (fetch and XMLHttpRequest
//! refuse other sites), it can't run code from strings (no eval, no
//! `setTimeout("code")`), it opens no windows, and everything it stores
//! lasts only until the browser closes.

use super::dom::{self, FetchFor, NavReq, STORAGE_QUOTA};
use super::interp::{Args, Ctrl, ErrKind, Interp, R};
use super::value::*;
use crate::net::{FetchError, Method, Request, Response, Url};

type A<'a> = Args<'a>;

const PRELUDE: &str = include_str!("prelude.js");

fn s(v: &str) -> Value {
    Value::str(v)
}

fn str_arg(it: &mut Interp, a: &A, i: usize) -> R<String> {
    Ok(it.to_str(&a.get(i))?.to_string())
}

/// One part of a URL, as Location and URL objects name them.
pub fn url_part(u: &Url, key: &str) -> String {
    match key {
        "href" => u.as_str().to_string(),
        "protocol" => format!("{}:", u.scheme()),
        "host" => match u.port() {
            Some(p) => format!("{}:{p}", u.host()),
            None => u.host().to_string(),
        },
        "hostname" => u.host().to_string(),
        "port" => u.port().map(|p| p.to_string()).unwrap_or_default(),
        "pathname" => u.path().to_string(),
        "search" => match u.query() {
            Some(q) if !q.is_empty() => format!("?{q}"),
            _ => String::new(),
        },
        "hash" => match u.fragment() {
            Some(f) if !f.is_empty() => format!("#{f}"),
            _ => String::new(),
        },
        "origin" => dom::origin_of(u),
        _ => String::new(),
    }
}

/// Asks the browser to go to `href` (relative to the page) after this task.
pub fn navigate(it: &mut Interp, href: &str, replace: bool) {
    let h = href.trim();
    if h.to_ascii_lowercase().starts_with("javascript:") {
        it.host.log("javascript: URLs are not followed".into());
        return;
    }
    match it.host.resolve(h) {
        Some(u) => it.host.nav = Some(NavReq::Go { url: u.as_str().to_string(), replace }),
        None => it.host.log(format!("can't go to {h}: not a valid address")),
    }
}

fn def(it: &mut Interp, name: &str, v: Value) {
    let g = it.global;
    it.hide(g, name, v);
}

fn global_getter(it: &mut Interp, name: &str, f: NativeFn) {
    let g = it.global;
    let gf = it.new_native(name, f, 0);
    // A setter that keeps the value scripts assign (window.name = ...).
    it.heap.get_mut(g).props.set(name.into(), Slot::Accessor(Some(gf), None, F_CONF));
}

pub fn install(it: &mut Interp) {
    let g = it.global;
    let gv = Value::Obj(g);
    for name in ["window", "self", "top", "parent", "frames"] {
        def(it, name, gv.clone());
    }
    dom::install_event_target(it, g);
    let doc = dom::wrap(it, crate::dom::Document::ROOT);
    def(it, "document", doc);
    for (k, v) in [("opener", Value::Null), ("frameElement", Value::Null), ("closed", Value::Bool(false)), ("length", Value::Num(0.0)), ("crossOriginIsolated", Value::Bool(false))] {
        def(it, k, v);
    }
    it.set_prop(g, "name", s(""));
    it.set_prop(g, "status", s(""));
    global_getter(it, "origin", |it, _| Ok(s(&it.host.origin())));
    global_getter(it, "isSecureContext", |it, _| Ok(Value::Bool(it.host.url.as_ref().is_some_and(|u| u.scheme() == "https"))));
    global_getter(it, "innerWidth", |it, _| Ok(Value::Num(it.host.viewport.0 as f64)));
    global_getter(it, "innerHeight", |it, _| Ok(Value::Num(it.host.viewport.1 as f64)));
    global_getter(it, "outerWidth", |it, _| Ok(Value::Num(it.host.viewport.0 as f64)));
    global_getter(it, "outerHeight", |it, _| Ok(Value::Num(it.host.viewport.1 as f64 + 52.0)));
    global_getter(it, "scrollX", |it, _| Ok(Value::Num(it.host.scroll.0 as f64)));
    global_getter(it, "pageXOffset", |it, _| Ok(Value::Num(it.host.scroll.0 as f64)));
    global_getter(it, "scrollY", |it, _| Ok(Value::Num(it.host.scroll.1 as f64)));
    global_getter(it, "pageYOffset", |it, _| Ok(Value::Num(it.host.scroll.1 as f64)));
    def(it, "devicePixelRatio", Value::Num(1.0));
    def(it, "screenX", Value::Num(0.0));
    def(it, "screenY", Value::Num(0.0));
    def(it, "screenLeft", Value::Num(0.0));
    def(it, "screenTop", Value::Num(0.0));

    install_timers(it);
    install_location(it);
    install_console(it);
    install_dialogs(it);
    install_storage(it);
    install_misc(it);
    install_natives(it);

    // The parts of the platform written in script.
    match super::parser::parse_program(PRELUDE) {
        Ok(p) => {
            it.start_task();
            if let Err(e) = it.run_program(&p) {
                let m = match e {
                    Ctrl::Throw(v) => dom::describe_error(it, &v),
                    other => format!("{other:?}"),
                };
                panic!("prelude.js failed: {m}");
            }
        }
        Err(e) => panic!("prelude.js doesn't parse: {e}"),
    }
}

// ---- timers ----------------------------------------------------------------------------

fn set_timer(it: &mut Interp, a: &A, every: bool, min: f64) -> R<Value> {
    let f = a.get(0);
    if !it.is_callable(&f) {
        if matches!(f, Value::Str(_)) {
            it.host.log("setTimeout/setInterval with a string of code is not supported (no code from strings)".into());
            return Ok(Value::Num(0.0));
        }
        return it.type_err("the timer callback is not a function");
    }
    if it.host.timers.len() >= dom::MAX_TIMERS {
        return Err(it.err(ErrKind::Range, "too many timers"));
    }
    let ms = it.to_num(&a.get(1))?;
    let ms = if ms.is_finite() { ms.clamp(0.0, 2_147_483_647.0) } else { 0.0 };
    let ms = if every { ms.max(min.max(10.0)) } else { ms.max(min) };
    let id = it.host.next_id;
    it.host.next_id = it.host.next_id.wrapping_add(1).max(1);
    let args = a.args.iter().skip(2).cloned().collect();
    let due = it.host.now() + ms;
    it.host.timers.push(dom::Timer { id, due, every: if every { Some(ms) } else { None }, f, args });
    Ok(Value::Num(id as f64))
}

fn clear_timer(it: &mut Interp, a: &A) -> R<Value> {
    let id = it.to_num(&a.get(0))?;
    if id.is_finite() && id > 0.0 {
        let id = id as u32;
        it.host.timers.retain(|t| t.id != id);
    }
    Ok(Value::Undef)
}

fn install_timers(it: &mut Interp) {
    let g = it.global;
    it.method(g, "setTimeout", |it, a| set_timer(it, a, false, 0.0), 2);
    it.method(g, "setInterval", |it, a| set_timer(it, a, true, 10.0), 2);
    it.method(g, "clearTimeout", clear_timer, 1);
    it.method(g, "clearInterval", clear_timer, 1);
    it.method(g, "requestAnimationFrame", |it, a| {
        let f = a.get(0);
        if !it.is_callable(&f) {
            return it.type_err("requestAnimationFrame: the callback is not a function");
        }
        if it.host.timers.len() >= dom::MAX_TIMERS {
            return Err(it.err(ErrKind::Range, "too many timers"));
        }
        let id = it.host.next_id;
        it.host.next_id = it.host.next_id.wrapping_add(1).max(1);
        let now = it.host.now();
        it.host.timers.push(dom::Timer { id, due: now + 16.0, every: None, f, args: vec![Value::Num(now + 16.0)] });
        Ok(Value::Num(id as f64))
    }, 1);
    it.method(g, "cancelAnimationFrame", clear_timer, 1);
    it.method(g, "requestIdleCallback", |it, a| {
        let f = a.get(0);
        if !it.is_callable(&f) {
            return it.type_err("requestIdleCallback: the callback is not a function");
        }
        let id = it.host.next_id;
        it.host.next_id = it.host.next_id.wrapping_add(1).max(1);
        let deadline = it.new_object();
        it.method(deadline, "timeRemaining", |_, _| Ok(Value::Num(10.0)), 0);
        it.set_prop(deadline, "didTimeout", Value::Bool(false));
        let due = it.host.now() + 50.0;
        it.host.timers.push(dom::Timer { id, due, every: None, f, args: vec![Value::Obj(deadline)] });
        Ok(Value::Num(id as f64))
    }, 1);
    it.method(g, "cancelIdleCallback", clear_timer, 1);
}

// ---- location & history ---------------------------------------------------------------

fn install_location(it: &mut Interp) {
    let loc = it.new_object();
    fn part(it: &mut Interp, a: &A) -> R<Value> {
        let key = match &it.heap.get(a.callee).props.get("name") {
            Some(Slot::Data(Value::Str(n), _)) => n.to_string(),
            _ => String::new(),
        };
        Ok(s(&match &it.host.url {
            Some(u) => url_part(u, &key),
            None => if key == "href" { "about:blank".to_string() } else { String::new() },
        }))
    }
    fn set_part(it: &mut Interp, a: &A) -> R<Value> {
        let key = match &it.heap.get(a.callee).props.get("name") {
            Some(Slot::Data(Value::Str(n), _)) => n.to_string(),
            _ => String::new(),
        };
        let v = str_arg(it, a, 0)?;
        let Some(cur) = it.host.url.clone() else { return Ok(Value::Undef) };
        let target = match key.as_str() {
            "href" => v,
            "hash" => {
                let base = cur.as_str().split('#').next().unwrap_or("").to_string();
                let f = v.trim_start_matches('#');
                format!("{base}#{f}")
            }
            "search" => {
                let base = cur.as_str().split(['?', '#']).next().unwrap_or("").to_string();
                let q = v.trim_start_matches('?');
                format!("{base}?{q}")
            }
            "pathname" => format!("{}{}", dom::origin_of(&cur), if v.starts_with('/') { v } else { format!("/{v}") }),
            _ => {
                it.host.log(format!("location.{key} can't be changed"));
                return Ok(Value::Undef);
            }
        };
        navigate(it, &target, false);
        Ok(Value::Undef)
    }
    for k in ["href", "protocol", "host", "hostname", "port", "pathname", "search", "hash", "origin"] {
        dom::accessor(it, loc, k, part, set_part);
    }
    it.method(loc, "assign", |it, a| {
        let h = str_arg(it, a, 0)?;
        navigate(it, &h, false);
        Ok(Value::Undef)
    }, 1);
    it.method(loc, "replace", |it, a| {
        let h = str_arg(it, a, 0)?;
        navigate(it, &h, true);
        Ok(Value::Undef)
    }, 1);
    it.method(loc, "reload", |it, _| {
        it.host.nav = Some(NavReq::Reload);
        Ok(Value::Undef)
    }, 0);
    it.method(loc, "toString", |it, _| Ok(s(&it.host.url.as_ref().map(|u| u.as_str().to_string()).unwrap_or_else(|| "about:blank".into()))), 0);
    let anc = it.new_array(Vec::new());
    it.set_prop(loc, "ancestorOrigins", anc);
    let g = it.global;
    // window.location = "..." navigates.
    let lv = Value::Obj(loc);
    let getf = it.new_native_data("location", |it, a| {
        Ok(match &it.heap.get(a.callee).kind {
            Kind::Native(n) => n.data.first().cloned().unwrap_or(Value::Undef),
            _ => Value::Undef,
        })
    }, 0, vec![lv]);
    let setf = it.new_native("location", |it, a| {
        let h = str_arg(it, a, 0)?;
        navigate(it, &h, false);
        Ok(Value::Undef)
    }, 1);
    it.heap.get_mut(g).props.set("location".into(), Slot::Accessor(Some(getf), Some(setf), F_CONF));

    let hist = it.new_object();
    it.set_prop(hist, "state", Value::Null);
    it.set_prop(hist, "scrollRestoration", s("auto"));
    global_getter_on(it, hist, "length", |_, _| Ok(Value::Num(1.0)));
    fn push(it: &mut Interp, a: &A, replace: bool) -> R<Value> {
        let state = a.get(0);
        let url = a.get(2);
        if !url.is_nullish() {
            let u = it.to_str(&url)?.to_string();
            let Some(new) = it.host.resolve(&u) else { return Err(it.err(ErrKind::Error, "SecurityError: not a valid address")) };
            let same = it.host.url.as_ref().is_some_and(|cur| dom::origin_of(cur) == dom::origin_of(&new));
            if !same {
                return Err(it.err(ErrKind::Error, "SecurityError: history can only change to an address on the same site"));
            }
            it.host.url = Some(new.clone());
            it.host.base = Some(new.clone());
            it.host.url_changed = Some((new, replace));
        }
        let this = a.this.clone();
        it.put(&this, "state", state)?;
        Ok(Value::Undef)
    }
    it.method(hist, "pushState", |it, a| push(it, a, false), 3);
    it.method(hist, "replaceState", |it, a| push(it, a, true), 3);
    it.method(hist, "back", |it, _| {
        it.host.nav = Some(NavReq::Back);
        Ok(Value::Undef)
    }, 0);
    it.method(hist, "forward", |it, _| {
        it.host.nav = Some(NavReq::Forward);
        Ok(Value::Undef)
    }, 0);
    it.method(hist, "go", |it, a| {
        let d = it.to_num(&a.get(0))?;
        it.host.nav = Some(if d < 0.0 {
            NavReq::Back
        } else if d > 0.0 {
            NavReq::Forward
        } else {
            NavReq::Reload
        });
        Ok(Value::Undef)
    }, 1);
    def(it, "history", Value::Obj(hist));
}

fn global_getter_on(it: &mut Interp, o: Gc, name: &str, f: NativeFn) {
    let gf = it.new_native(name, f, 0);
    it.heap.get_mut(o).props.set(name.into(), Slot::Accessor(Some(gf), None, F_CONF));
}

// ---- console & dialogs -----------------------------------------------------------------

/// How console.log shows a value.
fn show(it: &mut Interp, v: &Value, depth: usize) -> String {
    match v {
        Value::Str(s) => s.to_string(),
        Value::Obj(g) => {
            let kind = it.heap.get(*g).kind.clone();
            match kind {
                Kind::Error => dom::describe_error(it, v),
                Kind::Node(n) => match &it.host.doc.nodes.get(n).map(|x| x.data.clone()) {
                    Some(crate::dom::NodeData::Element(e)) => format!("<{}>", e.name),
                    Some(crate::dom::NodeData::Text(t)) => format!("#text \"{}\"", t.chars().take(40).collect::<String>()),
                    _ => "#document".into(),
                },
                Kind::Array(items) if depth < 2 => {
                    let parts: Vec<String> = items.iter().take(50).map(|x| show(it, x, depth + 1)).collect();
                    format!("[{}]", parts.join(", "))
                }
                Kind::Plain if depth < 2 => {
                    let keys = it.own_keys(*g, true).unwrap_or_default();
                    let mut parts = Vec::new();
                    for k in keys.into_iter().take(30) {
                        let x = match it.heap.get(*g).props.get(&k) {
                            Some(Slot::Data(x, _)) => x.clone(),
                            _ => Value::Undef,
                        };
                        let shown = match &x {
                            Value::Str(s) => format!("{:?}", s.as_ref()),
                            other => show(it, other, depth + 1),
                        };
                        parts.push(format!("{k}: {shown}"));
                    }
                    format!("{{{}}}", parts.join(", "))
                }
                _ if it.is_callable(v) => "function".into(),
                _ => dom::describe_error(it, v),
            }
        }
        other => it.to_str(other).map(|s| s.to_string()).unwrap_or_default(),
    }
}

fn log_line(it: &mut Interp, a: &A, prefix: &str) -> String {
    let mut parts = Vec::new();
    for v in a.args.iter().take(20) {
        parts.push(show(it, v, 0));
    }
    format!("{prefix}{}", parts.join(" "))
}

fn install_console(it: &mut Interp) {
    let c = it.new_object();
    for (name, prefix) in [("log", ""), ("info", ""), ("debug", ""), ("trace", ""), ("dir", ""), ("table", ""), ("group", ""), ("groupCollapsed", ""), ("warn", "warning: "), ("error", "error: ")] {
        let f: NativeFn = match prefix {
            "warning: " => |it, a| {
                let l = log_line(it, a, "warning: ");
                it.host.log(l);
                Ok(Value::Undef)
            },
            "error: " => |it, a| {
                let l = log_line(it, a, "error: ");
                it.host.log(l);
                Ok(Value::Undef)
            },
            _ => |it, a| {
                let l = log_line(it, a, "");
                it.host.log(l);
                Ok(Value::Undef)
            },
        };
        it.method(c, name, f, 0);
    }
    it.method(c, "assert", |it, a| {
        if !Interp::truthy(&a.get(0)) {
            let rest = A { this: a.this.clone(), args: a.args.get(1..).unwrap_or(&[]), callee: a.callee, new_target: None };
            let l = log_line(it, &rest, "assertion failed: ");
            it.host.log(l);
        }
        Ok(Value::Undef)
    }, 1);
    for name in ["groupEnd", "time", "timeEnd", "timeLog", "count", "countReset", "clear", "profile", "profileEnd"] {
        it.method(c, name, |_, _| Ok(Value::Undef), 0);
    }
    def(it, "console", Value::Obj(c));
}

fn install_dialogs(it: &mut Interp) {
    let g = it.global;
    it.method(g, "alert", |it, a| {
        let m = if a.args.is_empty() { String::new() } else { str_arg(it, a, 0)? };
        it.host.alerts.push(m);
        Ok(Value::Undef)
    }, 1);
    it.method(g, "confirm", |it, a| {
        let m = str_arg(it, a, 0)?;
        it.host.alerts.push(format!("{m} (the page asked to confirm; the browser answered no)"));
        Ok(Value::Bool(false))
    }, 1);
    it.method(g, "prompt", |it, a| {
        let m = str_arg(it, a, 0)?;
        it.host.alerts.push(format!("{m} (the page asked a question; the browser doesn't answer)"));
        Ok(Value::Null)
    }, 1);
    it.method(g, "open", |it, _| {
        it.host.alerts.push("The page tried to open a new window; that was blocked.".into());
        Ok(Value::Null)
    }, 0);
    for name in ["close", "print", "focus", "blur", "stop", "moveTo", "moveBy", "resizeTo", "resizeBy", "captureEvents", "releaseEvents"] {
        it.method(g, name, |_, _| Ok(Value::Undef), 0);
    }
    fn scroll(it: &mut Interp, a: &A, by: bool) -> R<Value> {
        let (mut x, mut y) = (it.host.scroll.0, it.host.scroll.1);
        let first = a.get(0);
        let (nx, ny) = if let Value::Obj(_) = first {
            let l = it.get(&first, "left")?;
            let t = it.get(&first, "top")?;
            (if l.is_nullish() { None } else { Some(it.to_num(&l)? as f32) }, if t.is_nullish() { None } else { Some(it.to_num(&t)? as f32) })
        } else {
            (Some(it.to_num(&first)? as f32), Some(it.to_num(&a.get(1))? as f32))
        };
        if by {
            x += nx.unwrap_or(0.0);
            y += ny.unwrap_or(0.0);
        } else {
            x = nx.unwrap_or(x);
            y = ny.unwrap_or(y);
        }
        if x.is_finite() && y.is_finite() {
            it.host.scroll_req = Some((x.max(0.0), y.max(0.0)));
        }
        Ok(Value::Undef)
    }
    it.method(g, "scrollTo", |it, a| scroll(it, a, false), 2);
    it.method(g, "scroll", |it, a| scroll(it, a, false), 2);
    it.method(g, "scrollBy", |it, a| scroll(it, a, true), 2);
    it.method(g, "postMessage", |it, a| {
        // Delivered to this window as a "message" event, after this task.
        let data = a.get(0);
        let f = it.get(&Value::Obj(it.global), "@@deliverMessage")?;
        if it.is_callable(&f) {
            let id = it.host.next_id;
            it.host.next_id = it.host.next_id.wrapping_add(1).max(1);
            let due = it.host.now();
            it.host.timers.push(dom::Timer { id, due, every: None, f, args: vec![data] });
        }
        Ok(Value::Undef)
    }, 2);
}

// ---- storage ---------------------------------------------------------------------------

fn storage_kind(it: &Interp, a: &A) -> bool {
    matches!(&it.heap.get(a.callee).kind, Kind::Native(n) if matches!(n.data.first(), Some(Value::Bool(true))))
}

fn with_store<T>(it: &mut Interp, local: bool, f: impl FnOnce(&mut Vec<(String, String)>) -> T) -> T {
    let o = it.host.origin();
    let mut st = it.host.storage.borrow_mut();
    let map = if local { &mut st.local } else { &mut st.session };
    f(map.entry(o).or_default())
}

fn install_storage(it: &mut Interp) {
    for (name, local) in [("localStorage", true), ("sessionStorage", false)] {
        let o = it.new_object();
        let tag = vec![Value::Bool(local)];
        let m = |it: &mut Interp, o: Gc, name: &str, f: NativeFn, n: u32, tag: &Vec<Value>| {
            let fun = it.new_native_data(name, f, n, tag.clone());
            it.hide(o, name, Value::Obj(fun));
        };
        m(it, o, "getItem", |it, a| {
            let local = storage_kind(it, a);
            let k = str_arg(it, a, 0)?;
            let v = with_store(it, local, |s| s.iter().find(|(x, _)| *x == k).map(|(_, v)| v.clone()));
            Ok(v.map(|v| s(&v)).unwrap_or(Value::Null))
        }, 1, &tag);
        m(it, o, "setItem", |it, a| {
            let local = storage_kind(it, a);
            let k = str_arg(it, a, 0)?;
            let v = str_arg(it, a, 1)?;
            let ok = with_store(it, local, |st| {
                let used: usize = st.iter().filter(|(x, _)| *x != k).map(|(x, v)| x.len() + v.len()).sum();
                if used + k.len() + v.len() > STORAGE_QUOTA {
                    return false;
                }
                match st.iter_mut().find(|(x, _)| *x == k) {
                    Some(e) => e.1 = v,
                    None => st.push((k, v)),
                }
                true
            });
            if !ok {
                return Err(it.err(ErrKind::Error, "QuotaExceededError: storage is full"));
            }
            Ok(Value::Undef)
        }, 2, &tag);
        m(it, o, "removeItem", |it, a| {
            let local = storage_kind(it, a);
            let k = str_arg(it, a, 0)?;
            with_store(it, local, |st| st.retain(|(x, _)| *x != k));
            Ok(Value::Undef)
        }, 1, &tag);
        m(it, o, "clear", |it, a| {
            let local = storage_kind(it, a);
            with_store(it, local, |st| st.clear());
            Ok(Value::Undef)
        }, 0, &tag);
        m(it, o, "key", |it, a| {
            let local = storage_kind(it, a);
            let i = it.to_num(&a.get(0))?;
            let k = with_store(it, local, |st| if i >= 0.0 { st.get(i as usize).map(|(k, _)| k.clone()) } else { None });
            Ok(k.map(|k| s(&k)).unwrap_or(Value::Null))
        }, 1, &tag);
        let lf = it.new_native_data("length", |it, a| {
            let local = storage_kind(it, a);
            Ok(Value::Num(with_store(it, local, |st| st.len()) as f64))
        }, 0, tag.clone());
        it.heap.get_mut(o).props.set("length".into(), Slot::Accessor(Some(lf), None, F_CONF));
        def(it, name, Value::Obj(o));
    }
}

// ---- navigator, screen, performance, media queries --------------------------------------

fn install_misc(it: &mut Interp) {
    let nav = it.new_object();
    for (k, v) in [
        ("userAgent", "Mozilla/5.0 (aether) aether-browser/0.7"),
        ("appName", "Netscape"),
        ("appVersion", "5.0 (aether)"),
        ("appCodeName", "Mozilla"),
        ("product", "Gecko"),
        ("platform", "aether"),
        ("vendor", ""),
        ("language", "en"),
        ("doNotTrack", "1"),
    ] {
        it.set_prop(nav, k, s(v));
    }
    let langs = it.new_array(vec![s("en")]);
    it.set_prop(nav, "languages", langs);
    for (k, v) in [("onLine", true), ("cookieEnabled", true), ("webdriver", false), ("pdfViewerEnabled", false), ("javaEnabled", false), ("globalPrivacyControl", true)] {
        it.set_prop(nav, k, Value::Bool(v));
    }
    it.set_prop(nav, "hardwareConcurrency", Value::Num(1.0));
    it.set_prop(nav, "maxTouchPoints", Value::Num(0.0));
    let plugins = it.new_array(Vec::new());
    it.set_prop(nav, "plugins", plugins.clone());
    it.set_prop(nav, "mimeTypes", plugins);
    // Beacons are how pages report on their visitors as they leave: refused.
    it.method(nav, "sendBeacon", |_, _| Ok(Value::Bool(false)), 2);
    it.method(nav, "vibrate", |_, _| Ok(Value::Bool(false)), 1);
    it.method(nav, "javaEnabled", |_, _| Ok(Value::Bool(false)), 0);
    def(it, "navigator", Value::Obj(nav));

    let scr = it.new_object();
    global_getter_on(it, scr, "width", |it, _| Ok(Value::Num(it.host.viewport.0 as f64)));
    global_getter_on(it, scr, "height", |it, _| Ok(Value::Num(it.host.viewport.1 as f64 + 52.0)));
    global_getter_on(it, scr, "availWidth", |it, _| Ok(Value::Num(it.host.viewport.0 as f64)));
    global_getter_on(it, scr, "availHeight", |it, _| Ok(Value::Num(it.host.viewport.1 as f64 + 52.0)));
    it.set_prop(scr, "colorDepth", Value::Num(24.0));
    it.set_prop(scr, "pixelDepth", Value::Num(24.0));
    let orient = it.new_object();
    it.set_prop(orient, "type", s("landscape-primary"));
    it.set_prop(orient, "angle", Value::Num(0.0));
    it.set_prop(scr, "orientation", Value::Obj(orient));
    def(it, "screen", Value::Obj(scr));

    let perf = it.new_object();
    it.method(perf, "now", |it, _| Ok(Value::Num((it.host.now() * 10.0).round() / 10.0)), 0);
    it.set_prop(perf, "timeOrigin", Value::Num(super::builtins::now_ms()));
    for name in ["mark", "measure", "clearMarks", "clearMeasures", "setResourceTimingBufferSize", "clearResourceTimings"] {
        it.method(perf, name, |_, _| Ok(Value::Undef), 1);
    }
    for name in ["getEntries", "getEntriesByType", "getEntriesByName"] {
        it.method(perf, name, |it, _| Ok(it.new_array(Vec::new())), 1);
    }
    let timing = it.new_object();
    it.set_prop(timing, "navigationStart", Value::Num(super::builtins::now_ms()));
    it.set_prop(perf, "timing", Value::Obj(timing));
    let pnav = it.new_object();
    it.set_prop(pnav, "type", Value::Num(0.0));
    it.set_prop(perf, "navigation", Value::Obj(pnav));
    def(it, "performance", Value::Obj(perf));

    let g = it.global;
    it.method(g, "matchMedia", |it, a| {
        let q = str_arg(it, a, 0)?;
        let cvs = crate::css::tokenizer::component_values(crate::css::tokenizer::tokenize(&q));
        let env = crate::css::MediaEnv { width: it.host.viewport.0, height: it.host.viewport.1 };
        let m = crate::css::parser::media_matches(&cvs, &env);
        let o = it.new_object();
        it.set_prop(o, "matches", Value::Bool(m));
        it.set_prop(o, "media", s(&q));
        it.set_prop(o, "onchange", Value::Null);
        it.method(o, "addListener", |_, _| Ok(Value::Undef), 1);
        it.method(o, "removeListener", |_, _| Ok(Value::Undef), 1);
        dom::install_event_target(it, o);
        Ok(Value::Obj(o))
    }, 1);
    it.method(g, "getComputedStyle", |it, a| {
        // Only the element's own style="" declarations are known here.
        let el = a.get(0);
        if dom::node_of(it, &el).is_none() {
            return it.type_err("getComputedStyle: not an element");
        }
        it.get(&el, "style")
    }, 1);
    it.method(g, "getSelection", |_, _| Ok(Value::Null), 0);
}

// ---- natives the prelude builds on ------------------------------------------------------

fn install_natives(it: &mut Interp) {
    let g = it.global;
    // Parts of a URL: an object, or a TypeError for an invalid one.
    it.method(g, "@@parseURL", |it, a| {
        let t = str_arg(it, a, 0)?;
        let base = a.get(1);
        let u = if base.is_nullish() {
            Url::parse(t.trim())
        } else {
            let b = it.to_str(&base)?.to_string();
            match Url::parse(b.trim()) {
                Ok(bu) => bu.join(t.trim()),
                Err(e) => Err(e),
            }
        };
        let Ok(u) = u else { return it.type_err(&format!("Invalid URL: {t}")) };
        let o = it.new_object();
        for k in ["href", "protocol", "host", "hostname", "port", "pathname", "search", "hash", "origin"] {
            it.set_prop(o, k, s(&url_part(&u, k)));
        }
        Ok(Value::Obj(o))
    }, 2);
    // Secure random bytes (the kernel's generator, as TLS uses).
    it.method(g, "@@random", |it, a| {
        let n = it.to_num(&a.get(0))?.clamp(0.0, 65536.0) as usize;
        let mut b = vec![0u8; n];
        webnet::crypto::random(&mut b);
        Ok(it.new_array(b.into_iter().map(|x| Value::Num(x as f64)).collect()))
    }, 1);
    // fetch(url, method, body) -> Promise of [status, url, headers, body text].
    it.method(g, "@@fetch", |it, a| {
        let req = make_request(it, a)?;
        let p = super::builtins::new_promise(it);
        match req {
            Ok(r) => dom::start_fetch(it, r, FetchFor::Promise(p)),
            Err(msg) => {
                let e = it.make_error(ErrKind::Type, &msg);
                super::builtins::settle(it, p, PState::Rejected(e));
            }
        }
        Ok(Value::Obj(p))
    }, 3);
    // XMLHttpRequest's send: (xhr, url, method, body, sync).
    it.method(g, "@@xhrSend", |it, a| {
        let x = a.get(0);
        let Value::Obj(xg) = x else { return it.type_err("not a request") };
        let rest = A { this: Value::Undef, args: a.args.get(1..).unwrap_or(&[]), callee: a.callee, new_target: None };
        let req = make_request(it, &rest)?;
        let sync = Interp::truthy(&a.get(4));
        match req {
            Ok(r) => {
                if sync {
                    let f = it.host.fetcher;
                    let resp = match f {
                        Some(f) => f(&r),
                        None => Err(FetchError::Timeout),
                    };
                    xhr_done(it, xg, resp)?;
                } else {
                    dom::start_fetch(it, r, FetchFor::Xhr(xg));
                }
            }
            Err(msg) => {
                let fail = it.get(&x, "@@fail")?;
                it.call(&fail, x.clone(), vec![s(&msg)])?;
            }
        }
        Ok(Value::Undef)
    }, 5);
    it.method(g, "@@def", |it, a| {
        let name = str_arg(it, a, 0)?;
        let v = a.get(1);
        let g = it.global;
        it.hide(g, &name, v);
        Ok(Value::Undef)
    }, 2);
}

/// A request a script may make: same origin only, GET/HEAD/POST.
fn make_request(it: &mut Interp, a: &A) -> R<Result<Request, String>> {
    let url = str_arg(it, a, 0)?;
    let method = if a.get(1).is_nullish() { "GET".to_string() } else { str_arg(it, a, 1)?.to_ascii_uppercase() };
    let body = if a.get(2).is_nullish() { Vec::new() } else { str_arg(it, a, 2)?.into_bytes() };
    let Some(u) = it.host.resolve(&url) else { return Ok(Err(format!("Failed to fetch: {url} is not a valid address"))) };
    let page = it.host.url.clone();
    let same = page.as_ref().is_some_and(|p| dom::origin_of(p) == dom::origin_of(&u));
    if !same {
        it.host.log(format!("blocked a request to {} (scripts may only contact their own site)", url_part(&u, "origin")));
        return Ok(Err(format!("Failed to fetch: requests to other sites are blocked ({})", url_part(&u, "origin"))));
    }
    let m = match method.as_str() {
        "GET" => Method::Get,
        "HEAD" => Method::Head,
        "POST" => Method::Post,
        other => return Ok(Err(format!("Failed to fetch: method {other} isn't supported"))),
    };
    let mut req = Request::get(u.clone());
    req.method = m;
    req.body = body;
    req.allow_http = u.scheme() == "http" && page.is_some_and(|p| p.scheme() == "http");
    req.max_bytes = 8 << 20;
    Ok(Ok(req))
}

fn response_parts(it: &mut Interp, r: &Response) -> Value {
    let text = crate::html::decode_as(r.body.strip_prefix(b"\xEF\xBB\xBF".as_slice()).unwrap_or(&r.body), crate::html::Charset::Utf8);
    let mut hs = Vec::new();
    for (k, v) in &r.headers {
        if k.eq_ignore_ascii_case("set-cookie") {
            continue;
        }
        let pair = it.new_array(vec![s(&k.to_ascii_lowercase()), s(v)]);
        hs.push(pair);
    }
    let headers = it.new_array(hs);
    it.new_array(vec![Value::Num(r.status as f64), s(r.url.as_str()), headers, s(&text)])
}

pub fn fetch_done(it: &mut Interp, p: Gc, r: Result<Response, FetchError>) -> R<()> {
    match r {
        Ok(resp) => {
            let parts = response_parts(it, &resp);
            super::builtins::resolve_promise(it, p, parts)?;
        }
        Err(e) => {
            let err = it.make_error(ErrKind::Type, &format!("Failed to fetch: {e}"));
            super::builtins::settle(it, p, PState::Rejected(err));
        }
    }
    Ok(())
}

pub fn xhr_done(it: &mut Interp, x: Gc, r: Result<Response, FetchError>) -> R<()> {
    let xv = Value::Obj(x);
    let r = match r {
        Ok(resp) => {
            let parts = response_parts(it, &resp);
            let done = it.get(&xv, "@@done")?;
            it.call(&done, xv.clone(), vec![parts])
        }
        Err(e) => {
            let fail = it.get(&xv, "@@fail")?;
            it.call(&fail, xv.clone(), vec![s(&e.to_string())])
        }
    };
    match r {
        Ok(_) => Ok(()),
        Err(Ctrl::Abort(m)) => Err(Ctrl::Abort(m)),
        Err(e) => {
            dom::report(it, e);
            Ok(())
        }
    }
}
