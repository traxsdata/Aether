//! script/value.rs - values, objects and the heap they live in.
//!
//! Objects live in an arena and are named by index (`Gc`). The arena
//! belongs to one page: leaving the page drops it whole. While the page is
//! open, a mark-and-sweep collector runs between tasks (never in the middle
//! of one), from the roots the runtime lists.

use std::collections::HashMap;
use std::rc::Rc;

use super::ast::Func;
use super::regex::Regex;
use crate::dom::NodeId;

pub type JsStr = Rc<str>;

#[derive(Clone, Debug, Default)]
pub enum Value {
    #[default]
    Undef,
    Null,
    Bool(bool),
    Num(f64),
    Str(JsStr),
    Obj(Gc),
}

impl Value {
    pub fn str(s: &str) -> Value {
        Value::Str(s.into())
    }
    pub fn is_nullish(&self) -> bool {
        matches!(self, Value::Undef | Value::Null)
    }
    pub fn as_obj(&self) -> Option<Gc> {
        match self {
            Value::Obj(g) => Some(*g),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Gc(pub u32);

pub const F_WRITE: u8 = 1;
pub const F_ENUM: u8 = 2;
pub const F_CONF: u8 = 4;
/// An ordinary assignment-created property.
pub const F_ALL: u8 = F_WRITE | F_ENUM | F_CONF;
/// Built-in methods: writable and configurable but not enumerable.
pub const F_HIDDEN: u8 = F_WRITE | F_CONF;

#[derive(Clone, Debug)]
pub enum Slot {
    Data(Value, u8),
    Accessor(Option<Gc>, Option<Gc>, u8),
}

impl Slot {
    pub fn flags(&self) -> u8 {
        match self {
            Slot::Data(_, f) | Slot::Accessor(_, _, f) => *f,
        }
    }
}

/// Properties in insertion order, with a hash index once there are many.
#[derive(Clone, Debug, Default)]
pub struct Props {
    keys: Vec<JsStr>,
    slots: Vec<Slot>,
    index: Option<HashMap<JsStr, usize>>,
}

const INDEX_AT: usize = 12;

impl Props {
    fn find(&self, k: &str) -> Option<usize> {
        match &self.index {
            Some(ix) => ix.get(k).copied(),
            None => self.keys.iter().position(|x| &**x == k),
        }
    }
    pub fn get(&self, k: &str) -> Option<&Slot> {
        self.find(k).map(|i| &self.slots[i])
    }
    pub fn get_mut(&mut self, k: &str) -> Option<&mut Slot> {
        self.find(k).map(|i| &mut self.slots[i])
    }
    pub fn set(&mut self, k: JsStr, s: Slot) {
        if let Some(i) = self.find(&k) {
            self.slots[i] = s;
            return;
        }
        if let Some(ix) = &mut self.index {
            ix.insert(k.clone(), self.keys.len());
        }
        self.keys.push(k);
        self.slots.push(s);
        if self.index.is_none() && self.keys.len() > INDEX_AT {
            self.index = Some(self.keys.iter().enumerate().map(|(i, k)| (k.clone(), i)).collect());
        }
    }
    pub fn remove(&mut self, k: &str) -> bool {
        let Some(i) = self.find(k) else { return false };
        self.keys.remove(i);
        self.slots.remove(i);
        if self.index.is_some() {
            self.index = Some(self.keys.iter().enumerate().map(|(i, k)| (k.clone(), i)).collect());
        }
        true
    }
    pub fn keys(&self) -> &[JsStr] {
        &self.keys
    }
    pub fn iter(&self) -> impl Iterator<Item = (&JsStr, &Slot)> {
        self.keys.iter().zip(self.slots.iter())
    }
    pub fn len(&self) -> usize {
        self.keys.len()
    }
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CtorKind {
    /// Arrows and methods can't be `new`ed.
    None,
    /// A plain `function`.
    Normal,
    /// A class constructor (can't be called without `new`).
    Base,
    Derived,
}

#[derive(Clone, Debug)]
pub struct Closure {
    pub def: Rc<Func>,
    pub env: Gc,
    /// For `super` inside methods.
    pub home: Option<Gc>,
    pub ctor: CtorKind,
    /// Class instance fields: (key, initialiser), run with `this` the new object.
    pub fields: Option<Rc<Vec<(JsStr, Option<super::ast::Expr>)>>>,
    /// The scope the fields' initialisers see.
    pub field_env: Option<Gc>,
}

pub type NativeFn = fn(&mut super::interp::Interp, &super::interp::Args) -> Result<Value, super::interp::Ctrl>;

#[derive(Clone)]
pub struct Native {
    pub f: NativeFn,
    pub data: Vec<Value>,
    pub ctor: bool,
}

impl std::fmt::Debug for Native {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Native")
    }
}

#[derive(Clone, Debug)]
pub struct Binding {
    pub v: Value,
    pub mutable: bool,
}

/// What a function's own scope carries besides its variables.
#[derive(Clone, Debug)]
pub struct FnScope {
    /// None until super() runs, in a derived class constructor.
    pub this: Option<Value>,
    pub func: Option<Gc>,
    pub new_target: Value,
    pub home: Option<Gc>,
}

#[derive(Clone, Debug)]
pub struct Env {
    pub vars: HashMap<JsStr, Binding>,
    pub parent: Option<Gc>,
    pub fun: Option<FnScope>,
}

#[derive(Clone, Debug)]
pub enum PState {
    Pending,
    Fulfilled(Value),
    Rejected(Value),
}

#[derive(Clone, Debug)]
pub struct Promise {
    pub state: PState,
    /// (on fulfilled, on rejected, the promise `then` returned)
    pub reactions: Vec<(Value, Value, Option<Gc>)>,
    pub handled: bool,
}

/// A Map or Set key, compared as SameValueZero.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum MapKey {
    Undef,
    Null,
    Bool(bool),
    Num(u64),
    Str(JsStr),
    Obj(Gc),
}

impl MapKey {
    pub fn of(v: &Value) -> MapKey {
        match v {
            Value::Undef => MapKey::Undef,
            Value::Null => MapKey::Null,
            Value::Bool(b) => MapKey::Bool(*b),
            Value::Num(n) => {
                let n = if *n == 0.0 { 0.0 } else { *n };
                MapKey::Num(if n.is_nan() { f64::NAN.to_bits() } else { n.to_bits() })
            }
            Value::Str(s) => MapKey::Str(s.clone()),
            Value::Obj(g) => MapKey::Obj(*g),
        }
    }
}

/// Insertion-ordered entries; deleted ones become None so iterators stay valid.
#[derive(Clone, Debug, Default)]
pub struct OrderedMap {
    pub entries: Vec<Option<(Value, Value)>>,
    pub index: HashMap<MapKey, usize>,
}

impl OrderedMap {
    pub fn get(&self, k: &Value) -> Option<&Value> {
        self.index.get(&MapKey::of(k)).and_then(|&i| self.entries[i].as_ref()).map(|e| &e.1)
    }
    pub fn set(&mut self, k: Value, v: Value) {
        let mk = MapKey::of(&k);
        if let Some(&i) = self.index.get(&mk) {
            if let Some(e) = &mut self.entries[i] {
                e.1 = v;
                return;
            }
        }
        // -0 is stored as +0.
        let k = match k {
            Value::Num(n) if n == 0.0 => Value::Num(0.0),
            k => k,
        };
        self.index.insert(mk, self.entries.len());
        self.entries.push(Some((k, v)));
    }
    pub fn delete(&mut self, k: &Value) -> bool {
        match self.index.remove(&MapKey::of(k)) {
            Some(i) => {
                self.entries[i] = None;
                true
            }
            None => false,
        }
    }
    pub fn clear(&mut self) {
        for e in &mut self.entries {
            *e = None;
        }
        self.index.clear();
    }
    pub fn len(&self) -> usize {
        self.index.len()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IterKind {
    Keys,
    Values,
    Entries,
}

#[derive(Clone, Debug)]
pub struct EventState {
    pub prevented: bool,
    pub stopped: bool,
    pub stopped_now: bool,
    pub cancelable: bool,
}

#[derive(Clone, Debug)]
pub enum Kind {
    Plain,
    Array(Vec<Value>),
    Func(Closure),
    Native(Native),
    Bound { target: Gc, this: Value, args: Vec<Value> },
    Error,
    Date(f64),
    RegExp(Rc<Regex>),
    Map(OrderedMap),
    Set(OrderedMap),
    Env(Env),
    Promise(Promise),
    /// A wrapper for a document node.
    Node(NodeId),
    ClassList(NodeId),
    Style(NodeId),
    Dataset(NodeId),
    Event(EventState),
    /// Iterates an array, Map or Set (by object) or a string.
    Iter { target: Value, pos: usize, kind: IterKind },
    /// A boxed primitive (`new String("x")` and friends).
    Boxed(Value),
}

#[derive(Clone, Debug)]
pub struct Obj {
    pub proto: Option<Gc>,
    pub props: Props,
    pub kind: Kind,
    pub extensible: bool,
}

impl Obj {
    pub fn new(proto: Option<Gc>, kind: Kind) -> Obj {
        Obj { proto, props: Props::default(), kind, extensible: true }
    }
    pub fn is_callable(&self) -> bool {
        matches!(self.kind, Kind::Func(_) | Kind::Native(_) | Kind::Bound { .. })
    }
}

#[derive(Default)]
pub struct Heap {
    objs: Vec<Option<Obj>>,
    free: Vec<u32>,
    pub live: usize,
}

impl Heap {
    pub fn alloc(&mut self, o: Obj) -> Gc {
        self.live += 1;
        if let Some(i) = self.free.pop() {
            self.objs[i as usize] = Some(o);
            return Gc(i);
        }
        self.objs.push(Some(o));
        Gc(self.objs.len() as u32 - 1)
    }
    pub fn get(&self, g: Gc) -> &Obj {
        self.objs[g.0 as usize].as_ref().expect("dangling object")
    }
    pub fn get_mut(&mut self, g: Gc) -> &mut Obj {
        self.objs[g.0 as usize].as_mut().expect("dangling object")
    }

    /// Frees everything not reachable from `roots`.
    pub fn collect(&mut self, roots: Vec<Gc>) {
        let mut marked = vec![false; self.objs.len()];
        let mut stack = roots;
        while let Some(g) = stack.pop() {
            let i = g.0 as usize;
            if i >= marked.len() || marked[i] {
                continue;
            }
            marked[i] = true;
            if let Some(o) = &self.objs[i] {
                trace(o, &mut stack);
            }
        }
        for (i, m) in marked.iter().enumerate() {
            if !m && self.objs[i].is_some() {
                self.objs[i] = None;
                self.free.push(i as u32);
                self.live -= 1;
            }
        }
    }
}

fn trace_v(v: &Value, out: &mut Vec<Gc>) {
    if let Value::Obj(g) = v {
        out.push(*g);
    }
}

fn trace(o: &Obj, out: &mut Vec<Gc>) {
    if let Some(p) = o.proto {
        out.push(p);
    }
    for (_, s) in o.props.iter() {
        match s {
            Slot::Data(v, _) => trace_v(v, out),
            Slot::Accessor(g, s, _) => {
                out.extend(g.iter().copied());
                out.extend(s.iter().copied());
            }
        }
    }
    match &o.kind {
        Kind::Array(v) => v.iter().for_each(|x| trace_v(x, out)),
        Kind::Func(c) => {
            out.push(c.env);
            out.extend(c.home);
            out.extend(c.field_env);
        }
        Kind::Native(n) => n.data.iter().for_each(|x| trace_v(x, out)),
        Kind::Bound { target, this, args } => {
            out.push(*target);
            trace_v(this, out);
            args.iter().for_each(|x| trace_v(x, out));
        }
        Kind::Map(m) | Kind::Set(m) => {
            for (k, v) in m.entries.iter().flatten() {
                trace_v(k, out);
                trace_v(v, out);
            }
        }
        Kind::Env(e) => {
            for b in e.vars.values() {
                trace_v(&b.v, out);
            }
            out.extend(e.parent);
            if let Some(f) = &e.fun {
                if let Some(t) = &f.this {
                    trace_v(t, out);
                }
                out.extend(f.func);
                out.extend(f.home);
                trace_v(&f.new_target, out);
            }
        }
        Kind::Promise(p) => {
            if let PState::Fulfilled(v) | PState::Rejected(v) = &p.state {
                trace_v(v, out);
            }
            for (a, b, c) in &p.reactions {
                trace_v(a, out);
                trace_v(b, out);
                out.extend(*c);
            }
        }
        Kind::Iter { target, .. } => trace_v(target, out),
        Kind::Boxed(v) => trace_v(v, out),
        _ => {}
    }
}

// ---- number <-> string ------------------------------------------------------

/// Number::toString(10), as the language defines it.
pub fn num_to_str(v: f64) -> String {
    if v.is_nan() {
        return "NaN".into();
    }
    if v == 0.0 {
        return "0".into();
    }
    if v.is_infinite() {
        return if v > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    if v < 0.0 {
        return format!("-{}", num_to_str(-v));
    }
    if v.fract() == 0.0 && v < 9007199254740992.0 {
        return format!("{v:.0}");
    }
    // Shortest round-trip digits, then the spec's layout rules.
    let e = format!("{v:e}");
    let (mant, exp) = e.split_once('e').unwrap_or((&e, "0"));
    let digits: String = mant.chars().filter(|c| c.is_ascii_digit()).collect();
    let k = digits.len() as i32;
    let n = exp.parse::<i32>().unwrap_or(0) + 1;
    if k <= n && n <= 21 {
        format!("{digits}{}", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 21 {
        format!("{}.{}", &digits[..n as usize], &digits[n as usize..])
    } else if -6 < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat((-n) as usize))
    } else {
        let sign = if n - 1 >= 0 { "+" } else { "-" };
        let ex = (n - 1).abs();
        if k == 1 {
            format!("{digits}e{sign}{ex}")
        } else {
            format!("{}.{}e{sign}{ex}", &digits[..1], &digits[1..])
        }
    }
}

/// StringToNumber.
pub fn str_to_num(s: &str) -> f64 {
    let t = s.trim_matches(|c: char| c.is_whitespace() || c == '\u{FEFF}');
    if t.is_empty() {
        return 0.0;
    }
    let lower = t.to_ascii_lowercase();
    for (p, r) in [("0x", 16), ("0o", 8), ("0b", 2)] {
        if let Some(rest) = lower.strip_prefix(p) {
            if rest.is_empty() {
                return f64::NAN;
            }
            let mut v = 0f64;
            for c in rest.chars() {
                match c.to_digit(r) {
                    Some(d) => v = v * r as f64 + d as f64,
                    None => return f64::NAN,
                }
            }
            return v;
        }
    }
    match t {
        "Infinity" | "+Infinity" => return f64::INFINITY,
        "-Infinity" => return f64::NEG_INFINITY,
        _ => {}
    }
    // Only decimal literals: Rust would also take "inf", "nan" and friends.
    if !t.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-')) {
        return f64::NAN;
    }
    t.parse::<f64>().unwrap_or(f64::NAN)
}

/// An array index key ("0", "17"; not "01" or "-1").
pub fn index_of_key(k: &str) -> Option<usize> {
    if k.is_empty() || k.len() > 10 || (k.len() > 1 && k.starts_with('0')) || !k.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let v: u64 = k.parse().ok()?;
    if v < u32::MAX as u64 {
        Some(v as usize)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers() {
        for (v, s) in [
            (1.0, "1"),
            (-1.5, "-1.5"),
            (0.1, "0.1"),
            (1e21, "1e+21"),
            (1.5e-7, "1.5e-7"),
            (123456789012345680000.0, "123456789012345680000"),
            (0.000001, "0.000001"),
            (1e-7, "1e-7"),
            (100.0, "100"),
            (f64::NAN, "NaN"),
            (-0.0, "0"),
            (0.1 + 0.2, "0.30000000000000004"),
        ] {
            assert_eq!(num_to_str(v), s);
        }
        assert_eq!(str_to_num(" 12 "), 12.0);
        assert_eq!(str_to_num("0x1F"), 31.0);
        assert!(str_to_num("inf").is_nan());
        assert!(str_to_num("12px").is_nan());
        assert_eq!(str_to_num(""), 0.0);
        assert_eq!(str_to_num("-Infinity"), f64::NEG_INFINITY);
        assert_eq!(index_of_key("12"), Some(12));
        assert_eq!(index_of_key("012"), None);
    }

    #[test]
    fn gc() {
        let mut h = Heap::default();
        let a = h.alloc(Obj::new(None, Kind::Plain));
        let b = h.alloc(Obj::new(None, Kind::Plain));
        let c = h.alloc(Obj::new(None, Kind::Array(vec![Value::Obj(a)])));
        h.get_mut(a).props.set("x".into(), Slot::Data(Value::Obj(c), F_ALL)); // a cycle
        h.collect(vec![c]);
        assert_eq!(h.live, 2);
        let _ = b;
        h.collect(vec![]);
        assert_eq!(h.live, 0);
    }
}
