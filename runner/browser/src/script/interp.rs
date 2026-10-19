//! script/interp.rs - the interpreter: walks the syntax tree.
//!
//! Everything a script does passes through here, and three limits apply to
//! all of it: a step budget per task (a runaway loop stops the script, not
//! the browser), a recursion limit (deep recursion is a RangeError, never a
//! crashed stack), and a cap on live objects and string lengths.

use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use super::ast::*;
use super::regex::Regex;
use super::value::*;

/// How control leaves an expression or statement other than normally.
#[derive(Debug, Clone)]
pub enum Ctrl {
    Throw(Value),
    Return(Value),
    Break(Option<Name>),
    Continue(Option<Name>),
    /// An optional chain met null or undefined.
    Short,
    /// A limit was hit: not catchable by the script.
    Abort(String),
}

pub type R<T> = Result<T, Ctrl>;

/// What a native function is called with.
pub struct Args<'a> {
    pub this: Value,
    pub args: &'a [Value],
    pub callee: Gc,
    pub new_target: Option<Value>,
}

impl Args<'_> {
    pub fn get(&self, i: usize) -> Value {
        self.args.get(i).cloned().unwrap_or(Value::Undef)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrKind {
    Error,
    Type,
    Range,
    Syntax,
    Reference,
}

/// Prototypes the interpreter needs to find quickly.
#[derive(Default, Clone)]
pub struct Protos {
    pub object: Option<Gc>,
    pub function: Option<Gc>,
    pub array: Option<Gc>,
    pub string: Option<Gc>,
    pub number: Option<Gc>,
    pub boolean: Option<Gc>,
    pub error: Option<Gc>,
    pub type_error: Option<Gc>,
    pub range_error: Option<Gc>,
    pub syntax_error: Option<Gc>,
    pub reference_error: Option<Gc>,
    pub regexp: Option<Gc>,
    pub date: Option<Gc>,
    pub map: Option<Gc>,
    pub set: Option<Gc>,
    pub promise: Option<Gc>,
    pub iter: Option<Gc>,
    pub node: Option<Gc>,
    pub event: Option<Gc>,
    pub class_list: Option<Gc>,
    pub style: Option<Gc>,
    pub dataset: Option<Gc>,
}

/// Steps one task (a script, an event, a timer) may take.
pub const TASK_STEPS: u64 = 30_000_000;
/// Nesting of calls and expressions (each unit is a few interpreter frames).
pub const MAX_DEPTH: usize = 700;
/// Live objects in one page.
pub const MAX_OBJECTS: usize = 2_000_000;
/// The longest string a script may build (characters).
pub const MAX_STRING: usize = 1 << 24;
/// The longest array.
pub const MAX_ARRAY: usize = 1 << 24;

pub struct Interp {
    pub heap: Heap,
    pub global: Gc,
    pub genv: Gc,
    pub env: Gc,
    pub p: Protos,
    pub steps: u64,
    pub step_limit: u64,
    depth: usize,
    pub microtasks: VecDeque<(Value, Vec<Value>)>,
    regex_cache: HashMap<(JsStr, JsStr), Rc<Regex>>,
    /// Promise rejections nobody handled during this task (for the console).
    pub unhandled: Vec<Gc>,
    pub host: super::dom::Host,
    /// Values kept alive for the host (listeners, timers...); see roots().
    pub extra_roots: Vec<Value>,
}

enum Ref {
    Var(Name),
    Prop(Value, JsStr),
}

fn is_index_num(n: f64) -> Option<usize> {
    if n >= 0.0 && n.fract() == 0.0 && n < u32::MAX as f64 {
        Some(n as usize)
    } else {
        None
    }
}

impl Interp {
    pub fn new(host: super::dom::Host) -> Interp {
        let mut heap = Heap::default();
        let global = heap.alloc(Obj::new(None, Kind::Plain));
        let genv = heap.alloc(Obj::new(
            None,
            Kind::Env(Env { vars: HashMap::new(), parent: None, fun: Some(FnScope { this: Some(Value::Obj(global)), func: None, new_target: Value::Undef, home: None }) }),
        ));
        let mut it = Interp {
            heap,
            global,
            genv,
            env: genv,
            p: Protos::default(),
            steps: 0,
            step_limit: TASK_STEPS,
            depth: 0,
            microtasks: VecDeque::new(),
            regex_cache: HashMap::new(),
            unhandled: Vec::new(),
            host,
            extra_roots: Vec::new(),
        };
        super::builtins::install(&mut it);
        super::dom::install(&mut it);
        it
    }

    // ---- limits -------------------------------------------------------------

    pub fn tick(&mut self) -> R<()> {
        self.steps += 1;
        if self.steps > self.step_limit {
            return Err(Ctrl::Abort("the script ran too long".into()));
        }
        if self.heap.live > MAX_OBJECTS {
            return Err(Ctrl::Abort("the script used too much memory".into()));
        }
        Ok(())
    }

    pub fn leave(&mut self) {
        self.depth -= 1;
    }

    /// Counts one level of recursion (natives that recurse use it too).
    pub fn enter(&mut self) -> R<()> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            self.depth -= 1;
            return Err(self.err(ErrKind::Range, "Maximum call stack size exceeded"));
        }
        Ok(())
    }

    pub fn start_task(&mut self) {
        self.step_limit = self.steps + TASK_STEPS;
        self.depth = 0;
        self.env = self.genv;
    }

    // ---- allocation -----------------------------------------------------------

    pub fn alloc(&mut self, o: Obj) -> Gc {
        self.heap.alloc(o)
    }

    pub fn new_object(&mut self) -> Gc {
        let p = self.p.object;
        self.alloc(Obj::new(p, Kind::Plain))
    }

    pub fn new_array(&mut self, v: Vec<Value>) -> Value {
        let p = self.p.array;
        Value::Obj(self.alloc(Obj::new(p, Kind::Array(v))))
    }

    pub fn new_native(&mut self, name: &str, f: NativeFn, len: u32) -> Gc {
        self.new_native_data(name, f, len, Vec::new())
    }

    pub fn new_native_data(&mut self, name: &str, f: NativeFn, len: u32, data: Vec<Value>) -> Gc {
        let p = self.p.function;
        let mut o = Obj::new(p, Kind::Native(Native { f, data, ctor: false }));
        o.props.set("name".into(), Slot::Data(Value::str(name), F_CONF));
        o.props.set("length".into(), Slot::Data(Value::Num(len as f64), F_CONF));
        self.alloc(o)
    }

    /// Defines a hidden (non-enumerable) data property.
    pub fn hide(&mut self, o: Gc, k: &str, v: Value) {
        self.heap.get_mut(o).props.set(k.into(), Slot::Data(v, F_HIDDEN));
    }

    pub fn method(&mut self, o: Gc, name: &str, f: NativeFn, len: u32) -> Gc {
        let fun = self.new_native(name, f, len);
        self.hide(o, name, Value::Obj(fun));
        fun
    }

    pub fn getter(&mut self, o: Gc, name: &str, f: NativeFn) {
        let fun = self.new_native(name, f, 0);
        self.heap.get_mut(o).props.set(name.into(), Slot::Accessor(Some(fun), None, F_CONF));
    }

    pub fn set_prop(&mut self, o: Gc, k: &str, v: Value) {
        self.heap.get_mut(o).props.set(k.into(), Slot::Data(v, F_ALL));
    }

    pub fn err(&mut self, kind: ErrKind, msg: &str) -> Ctrl {
        Ctrl::Throw(self.make_error(kind, msg))
    }

    pub fn make_error(&mut self, kind: ErrKind, msg: &str) -> Value {
        let proto = match kind {
            ErrKind::Error => self.p.error,
            ErrKind::Type => self.p.type_error,
            ErrKind::Range => self.p.range_error,
            ErrKind::Syntax => self.p.syntax_error,
            ErrKind::Reference => self.p.reference_error,
        };
        let mut o = Obj::new(proto, Kind::Error);
        o.props.set("message".into(), Slot::Data(Value::str(msg), F_HIDDEN));
        o.props.set("stack".into(), Slot::Data(Value::str(""), F_HIDDEN));
        Value::Obj(self.alloc(o))
    }

    pub fn type_err<T>(&mut self, msg: &str) -> R<T> {
        Err(self.err(ErrKind::Type, msg))
    }

    pub fn regex(&mut self, src: &JsStr, flags: &JsStr) -> R<Rc<Regex>> {
        if let Some(r) = self.regex_cache.get(&(src.clone(), flags.clone())) {
            return Ok(r.clone());
        }
        match Regex::new(src, flags) {
            Ok(r) => {
                let r = Rc::new(r);
                if self.regex_cache.len() < 1000 {
                    self.regex_cache.insert((src.clone(), flags.clone()), r.clone());
                }
                Ok(r)
            }
            Err(e) => Err(self.err(ErrKind::Syntax, &format!("Invalid regular expression: /{src}/: {e}"))),
        }
    }

    pub fn new_regexp(&mut self, re: Rc<Regex>) -> Value {
        let p = self.p.regexp;
        let mut o = Obj::new(p, Kind::RegExp(re));
        o.props.set("lastIndex".into(), Slot::Data(Value::Num(0.0), F_WRITE));
        Value::Obj(self.alloc(o))
    }

    // ---- conversions --------------------------------------------------------------

    pub fn truthy(v: &Value) -> bool {
        match v {
            Value::Undef | Value::Null => false,
            Value::Bool(b) => *b,
            Value::Num(n) => *n != 0.0 && !n.is_nan(),
            Value::Str(s) => !s.is_empty(),
            Value::Obj(_) => true,
        }
    }

    pub fn to_primitive(&mut self, v: &Value, hint_string: bool) -> R<Value> {
        let Value::Obj(g) = v else { return Ok(v.clone()) };
        if let Kind::Boxed(b) = &self.heap.get(*g).kind {
            return Ok(b.clone());
        }
        let hint_string = hint_string || matches!(self.heap.get(*g).kind, Kind::Date(_));
        let order = if hint_string { ["toString", "valueOf"] } else { ["valueOf", "toString"] };
        for m in order {
            let f = self.get(v, m)?;
            if self.is_callable(&f) {
                let r = self.call(&f, v.clone(), Vec::new())?;
                if !matches!(r, Value::Obj(_)) {
                    return Ok(r);
                }
            }
        }
        self.type_err("Cannot convert object to primitive value")
    }

    pub fn to_str(&mut self, v: &Value) -> R<JsStr> {
        Ok(match v {
            Value::Undef => "undefined".into(),
            Value::Null => "null".into(),
            Value::Bool(b) => (if *b { "true" } else { "false" }).into(),
            Value::Num(n) => num_to_str(*n).into(),
            Value::Str(s) => s.clone(),
            Value::Obj(_) => {
                let p = self.to_primitive(v, true)?;
                return self.to_str(&p);
            }
        })
    }

    pub fn to_num(&mut self, v: &Value) -> R<f64> {
        Ok(match v {
            Value::Undef => f64::NAN,
            Value::Null => 0.0,
            Value::Bool(b) => *b as u8 as f64,
            Value::Num(n) => *n,
            Value::Str(s) => str_to_num(s),
            Value::Obj(_) => {
                let p = self.to_primitive(v, false)?;
                return self.to_num(&p);
            }
        })
    }

    pub fn to_int32(&mut self, v: &Value) -> R<i32> {
        Ok(f64_to_int32(self.to_num(v)?))
    }

    /// A property key.
    pub fn to_key(&mut self, v: &Value) -> R<JsStr> {
        match v {
            Value::Str(s) => Ok(s.clone()),
            Value::Num(n) => Ok(num_to_str(*n).into()),
            _ => {
                let p = self.to_primitive(v, true)?;
                self.to_str(&p)
            }
        }
    }

    pub fn type_of(&self, v: &Value) -> &'static str {
        match v {
            Value::Undef => "undefined",
            Value::Null => "object",
            Value::Bool(_) => "boolean",
            Value::Num(_) => "number",
            Value::Str(_) => "string",
            Value::Obj(g) => {
                if self.heap.get(*g).is_callable() {
                    "function"
                } else {
                    "object"
                }
            }
        }
    }

    pub fn is_callable(&self, v: &Value) -> bool {
        matches!(v, Value::Obj(g) if self.heap.get(*g).is_callable())
    }

    pub fn strict_eq(a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::Undef, Value::Undef) | (Value::Null, Value::Null) => true,
            (Value::Bool(x), Value::Bool(y)) => x == y,
            (Value::Num(x), Value::Num(y)) => x == y,
            (Value::Str(x), Value::Str(y)) => x == y,
            (Value::Obj(x), Value::Obj(y)) => x == y,
            _ => false,
        }
    }

    pub fn same_value_zero(a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::Num(x), Value::Num(y)) if x.is_nan() && y.is_nan() => true,
            _ => Self::strict_eq(a, b),
        }
    }

    pub fn loose_eq(&mut self, a: &Value, b: &Value) -> R<bool> {
        Ok(match (a, b) {
            (Value::Undef | Value::Null, Value::Undef | Value::Null) => true,
            (Value::Undef | Value::Null, _) | (_, Value::Undef | Value::Null) => false,
            (Value::Num(_), Value::Str(_)) | (Value::Str(_), Value::Num(_)) => self.to_num(a)? == self.to_num(b)?,
            (Value::Bool(_), _) => {
                let n = Value::Num(self.to_num(a)?);
                return self.loose_eq(&n, b);
            }
            (_, Value::Bool(_)) => {
                let n = Value::Num(self.to_num(b)?);
                return self.loose_eq(a, &n);
            }
            (Value::Obj(_), Value::Obj(_)) => Self::strict_eq(a, b),
            (Value::Obj(_), _) => {
                let p = self.to_primitive(a, false)?;
                return self.loose_eq(&p, b);
            }
            (_, Value::Obj(_)) => {
                let p = self.to_primitive(b, false)?;
                return self.loose_eq(a, &p);
            }
            _ => Self::strict_eq(a, b),
        })
    }

    // ---- properties -----------------------------------------------------------------

    fn proto_of_primitive(&mut self, base: &Value, key: &str) -> R<Gc> {
        Ok(match base {
            Value::Obj(g) => *g,
            Value::Str(_) => self.p.string.unwrap(),
            Value::Num(_) => self.p.number.unwrap(),
            Value::Bool(_) => self.p.boolean.unwrap(),
            Value::Undef | Value::Null => {
                let what = if matches!(base, Value::Undef) { "undefined" } else { "null" };
                return self.type_err(&format!("Cannot read properties of {what} (reading '{key}')"));
            }
        })
    }

    pub fn get(&mut self, base: &Value, key: &str) -> R<Value> {
        if let Value::Str(s) = base {
            if key == "length" {
                return Ok(Value::Num(str_len(s) as f64));
            }
            if let Some(i) = index_of_key(key) {
                return Ok(str_at(s, i).map(|c| Value::Str(c.to_string().into())).unwrap_or(Value::Undef));
            }
        }
        let start = self.proto_of_primitive(base, key)?;
        self.get_from(start, key, base)
    }

    /// `base[key]` with a fast path for array indices.
    pub fn get_v(&mut self, base: &Value, key: &Value) -> R<Value> {
        if let (Value::Obj(g), Value::Num(n)) = (base, key) {
            if let Some(i) = is_index_num(*n) {
                if let Kind::Array(v) = &self.heap.get(*g).kind {
                    return Ok(v.get(i).cloned().unwrap_or(Value::Undef));
                }
            }
        }
        if let (Value::Str(s), Value::Num(n)) = (base, key) {
            if let Some(i) = is_index_num(*n) {
                return Ok(str_at(s, i).map(|c| Value::Str(c.to_string().into())).unwrap_or(Value::Undef));
            }
        }
        let k = self.to_key(key)?;
        self.get(base, &k)
    }

    pub fn get_from(&mut self, obj: Gc, key: &str, receiver: &Value) -> R<Value> {
        let mut cur = Some(obj);
        let mut own = true;
        let mut hops = 0;
        while let Some(g) = cur {
            hops += 1;
            if hops > 10_000 {
                break;
            }
            // Exotic objects first.
            match &self.heap.get(g).kind {
                Kind::Array(v) => {
                    if key == "length" {
                        return Ok(Value::Num(v.len() as f64));
                    }
                    if let Some(i) = index_of_key(key) {
                        if let Some(x) = v.get(i) {
                            return Ok(x.clone());
                        }
                    }
                }
                Kind::Boxed(Value::Str(s)) => {
                    if key == "length" {
                        return Ok(Value::Num(str_len(s) as f64));
                    }
                }
                Kind::Node(_) | Kind::ClassList(_) | Kind::Style(_) | Kind::Dataset(_) if own => {
                    if let Some(v) = super::dom::host_get(self, g, key)? {
                        return Ok(v);
                    }
                }
                Kind::Func(c) if key == "prototype" && own && c.ctor == CtorKind::Normal && self.heap.get(g).props.get("prototype").is_none() => {
                    // Made on first use: most functions are never constructors.
                    let proto = self.new_object();
                    self.hide(proto, "constructor", Value::Obj(g));
                    self.heap.get_mut(g).props.set("prototype".into(), Slot::Data(Value::Obj(proto), F_WRITE));
                    return Ok(Value::Obj(proto));
                }
                _ => {}
            }
            let o = self.heap.get(g);
            match o.props.get(key) {
                Some(Slot::Data(v, _)) => return Ok(v.clone()),
                Some(Slot::Accessor(get, _, _)) => {
                    return match get {
                        Some(f) => {
                            let f = Value::Obj(*f);
                            self.call(&f, receiver.clone(), Vec::new())
                        }
                        None => Ok(Value::Undef),
                    };
                }
                None => {}
            }
            cur = o.proto;
            own = false;
        }
        Ok(Value::Undef)
    }

    pub fn put(&mut self, base: &Value, key: &str, val: Value) -> R<()> {
        let g = match base {
            Value::Obj(g) => *g,
            Value::Undef | Value::Null => {
                let what = if matches!(base, Value::Undef) { "undefined" } else { "null" };
                return self.type_err(&format!("Cannot set properties of {what} (setting '{key}')"));
            }
            _ => return Ok(()),
        };
        // Exotic objects.
        match &mut self.heap.get_mut(g).kind {
            Kind::Array(v) => {
                if key == "length" {
                    let n = match &val {
                        Value::Num(n) => *n,
                        _ => {
                            let v2 = val.clone();
                            self.to_num(&v2)?
                        }
                    };
                    let Some(n) = is_index_num(n).filter(|&n| n <= MAX_ARRAY) else {
                        return Err(self.err(ErrKind::Range, "Invalid array length"));
                    };
                    if let Kind::Array(v) = &mut self.heap.get_mut(g).kind {
                        v.resize(n, Value::Undef);
                    }
                    return Ok(());
                }
                if let Some(i) = index_of_key(key) {
                    if i >= MAX_ARRAY {
                        return Err(self.err(ErrKind::Range, "Invalid array length"));
                    }
                    if i >= v.len() {
                        v.resize(i + 1, Value::Undef);
                    }
                    v[i] = val;
                    return Ok(());
                }
            }
            Kind::Node(_) | Kind::ClassList(_) | Kind::Style(_) | Kind::Dataset(_) => {
                if super::dom::host_set(self, g, key, &val)? {
                    return Ok(());
                }
            }
            _ => {}
        }
        // An own writable data property: update in place.
        if let Some(Slot::Data(v, f)) = self.heap.get_mut(g).props.get_mut(key) {
            if *f & F_WRITE != 0 {
                *v = val;
            }
            return Ok(());
        }
        // A setter (or a read-only property) up the chain.
        let mut cur = self.heap.get(g).props.get(key).map(|_| g).or(self.heap.get(g).proto);
        let mut hops = 0;
        while let Some(p) = cur {
            hops += 1;
            if hops > 10_000 {
                break;
            }
            match self.heap.get(p).props.get(key) {
                Some(Slot::Accessor(_, set, _)) => {
                    if let Some(s) = set {
                        let s = Value::Obj(*s);
                        self.call(&s, base.clone(), vec![val])?;
                    }
                    return Ok(());
                }
                Some(Slot::Data(_, f)) => {
                    if f & F_WRITE == 0 {
                        return Ok(());
                    }
                    break;
                }
                None => cur = self.heap.get(p).proto,
            }
        }
        if !self.heap.get(g).extensible {
            return Ok(());
        }
        self.heap.get_mut(g).props.set(key.into(), Slot::Data(val, F_ALL));
        Ok(())
    }

    pub fn put_v(&mut self, base: &Value, key: &Value, val: Value) -> R<()> {
        if let (Value::Obj(g), Value::Num(n)) = (base, key) {
            if let Some(i) = is_index_num(*n) {
                if let Kind::Array(v) = &mut self.heap.get_mut(*g).kind {
                    if i < v.len() {
                        v[i] = val;
                        return Ok(());
                    }
                    if i < MAX_ARRAY {
                        v.resize(i + 1, Value::Undef);
                        v[i] = val;
                        return Ok(());
                    }
                }
            }
        }
        let k = self.to_key(key)?;
        self.put(base, &k, val)
    }

    /// Defines (or redefines) an own data property, ignoring setters.
    pub fn define(&mut self, o: Gc, key: &str, v: Value) {
        if let Kind::Array(arr) = &mut self.heap.get_mut(o).kind {
            if let Some(i) = index_of_key(key) {
                if i < MAX_ARRAY {
                    if i >= arr.len() {
                        arr.resize(i + 1, Value::Undef);
                    }
                    arr[i] = v;
                }
                return;
            }
        }
        self.heap.get_mut(o).props.set(key.into(), Slot::Data(v, F_ALL));
    }

    pub fn has_own(&mut self, o: Gc, key: &str) -> R<bool> {
        match &self.heap.get(o).kind {
            Kind::Array(v) => {
                if key == "length" {
                    return Ok(true);
                }
                if let Some(i) = index_of_key(key) {
                    if i < v.len() {
                        return Ok(true);
                    }
                }
            }
            Kind::Node(_) | Kind::Style(_) | Kind::ClassList(_) | Kind::Dataset(_) => {
                if super::dom::host_get(self, o, key)?.is_some() {
                    return Ok(true);
                }
            }
            _ => {}
        }
        Ok(self.heap.get(o).props.get(key).is_some())
    }

    pub fn has_property(&mut self, o: Gc, key: &str) -> R<bool> {
        let mut cur = Some(o);
        let mut hops = 0;
        while let Some(g) = cur {
            hops += 1;
            if hops > 10_000 {
                break;
            }
            if self.has_own(g, key)? {
                return Ok(true);
            }
            cur = self.heap.get(g).proto;
        }
        Ok(false)
    }

    pub fn delete(&mut self, base: &Value, key: &str) -> R<bool> {
        let Value::Obj(g) = base else { return Ok(true) };
        if let Kind::Array(v) = &mut self.heap.get_mut(*g).kind {
            if let Some(i) = index_of_key(key) {
                if i < v.len() {
                    v[i] = Value::Undef;
                }
                return Ok(true);
            }
        }
        if let Some(s) = self.heap.get(*g).props.get(key) {
            if s.flags() & F_CONF == 0 {
                return Ok(false);
            }
        }
        self.heap.get_mut(*g).props.remove(key);
        Ok(true)
    }

    /// Own enumerable string keys, indices first.
    pub fn own_keys(&mut self, o: Gc, only_enumerable: bool) -> R<Vec<JsStr>> {
        let mut out: Vec<JsStr> = Vec::new();
        match &self.heap.get(o).kind {
            Kind::Array(v) => out.extend((0..v.len()).map(|i| JsStr::from(i.to_string()))),
            Kind::Boxed(Value::Str(s)) => out.extend((0..str_len(s)).map(|i| JsStr::from(i.to_string()))),
            Kind::Dataset(_) | Kind::Style(_) => {
                out.extend(super::dom::host_keys(self, o));
            }
            _ => {}
        }
        for (k, s) in self.heap.get(o).props.iter() {
            if (!only_enumerable || s.flags() & F_ENUM != 0) && !k.starts_with("@@") {
                out.push(k.clone());
            }
        }
        Ok(out)
    }

    // ---- calls --------------------------------------------------------------------------

    pub fn call(&mut self, f: &Value, this: Value, args: Vec<Value>) -> R<Value> {
        let Value::Obj(g) = f else {
            return self.type_err(&format!("{} is not a function", self.describe(f)));
        };
        self.tick()?;
        self.enter()?;
        let r = self.call_obj(*g, this, args, None);
        self.depth -= 1;
        r
    }

    fn describe(&self, v: &Value) -> String {
        match v {
            Value::Str(s) => format!("\"{}\"", s.chars().take(40).collect::<String>()),
            Value::Num(n) => num_to_str(*n),
            Value::Obj(_) => "object".into(),
            Value::Undef => "undefined".into(),
            Value::Null => "null".into(),
            Value::Bool(b) => b.to_string(),
        }
    }

    fn call_obj(&mut self, g: Gc, this: Value, args: Vec<Value>, new_target: Option<Value>) -> R<Value> {
        let kind = match &self.heap.get(g).kind {
            Kind::Func(c) => Ok(c.clone()),
            Kind::Native(n) => Err(Some((n.f, n.ctor))),
            Kind::Bound { .. } => Err(None),
            _ => return self.type_err("object is not a function"),
        };
        match kind {
            Ok(c) => {
                if matches!(c.ctor, CtorKind::Base | CtorKind::Derived) && new_target.is_none() {
                    return self.type_err("Class constructor cannot be invoked without 'new'");
                }
                let nt = new_target.unwrap_or(Value::Undef);
                self.invoke(g, &c, this, args, nt).map(|(v, _)| v)
            }
            Err(Some((f, is_ctor))) => {
                if new_target.is_some() && !is_ctor {
                    return self.type_err("not a constructor");
                }
                f(self, &Args { this, args: &args, callee: g, new_target })
            }
            Err(None) => {
                let Kind::Bound { target, this: bthis, args: bargs } = self.heap.get(g).kind.clone() else { unreachable!() };
                let mut all = bargs;
                all.extend(args);
                match new_target {
                    Some(nt) => {
                        let nt = if matches!(nt, Value::Obj(x) if x == g) { Value::Obj(target) } else { nt };
                        self.construct(&Value::Obj(target), all, Some(nt))
                    }
                    None => self.call_obj(target, bthis, all, None),
                }
            }
        }
    }

    pub fn construct(&mut self, f: &Value, args: Vec<Value>, new_target: Option<Value>) -> R<Value> {
        let Value::Obj(g) = f else {
            return self.type_err(&format!("{} is not a constructor", self.describe(f)));
        };
        let g = *g;
        self.tick()?;
        self.enter()?;
        let r = self.construct_inner(g, args, new_target);
        self.depth -= 1;
        r
    }

    fn construct_inner(&mut self, g: Gc, args: Vec<Value>, new_target: Option<Value>) -> R<Value> {
        let nt = new_target.unwrap_or(Value::Obj(g));
        let c = match &self.heap.get(g).kind {
            Kind::Func(c) => c.clone(),
            Kind::Native(n) if n.ctor => return self.call_obj(g, Value::Undef, args, Some(nt)),
            Kind::Bound { .. } => return self.call_obj(g, Value::Undef, args, Some(nt)),
            _ => return self.type_err("not a constructor"),
        };
        match c.ctor {
            CtorKind::None => self.type_err(&format!("{} is not a constructor", c.def.name.as_deref().unwrap_or("function"))),
            CtorKind::Normal | CtorKind::Base => {
                let proto = match self.get(&nt, "prototype")? {
                    Value::Obj(p) => Some(p),
                    _ => self.p.object,
                };
                let this = self.alloc(Obj::new(proto, Kind::Plain));
                if c.ctor == CtorKind::Base {
                    self.init_fields(g, this)?;
                }
                let (r, _) = self.invoke(g, &c, Value::Obj(this), args, nt)?;
                Ok(if matches!(r, Value::Obj(_)) { r } else { Value::Obj(this) })
            }
            CtorKind::Derived => {
                let (r, env) = self.invoke(g, &c, Value::Undef, args, nt)?;
                if matches!(r, Value::Obj(_)) {
                    return Ok(r);
                }
                match &self.heap.get(env).kind {
                    Kind::Env(Env { fun: Some(FnScope { this: Some(t), .. }), .. }) => Ok(t.clone()),
                    _ => Err(self.err(ErrKind::Reference, "Must call super constructor in derived class before accessing 'this' or returning from derived constructor")),
                }
            }
        }
    }

    /// Runs class field initialisers of constructor `ctor` on `this`.
    fn init_fields(&mut self, ctor: Gc, this: Gc) -> R<()> {
        let (fields, fenv) = match &self.heap.get(ctor).kind {
            Kind::Func(Closure { fields: Some(f), field_env, .. }) => (f.clone(), *field_env),
            _ => return Ok(()),
        };
        let home = match &self.heap.get(ctor).kind {
            Kind::Func(c) => c.home,
            _ => None,
        };
        for (k, init) in fields.iter() {
            let v = match init {
                Some(e) => {
                    let env = self.alloc(Obj::new(
                        None,
                        Kind::Env(Env { vars: HashMap::new(), parent: fenv, fun: Some(FnScope { this: Some(Value::Obj(this)), func: Some(ctor), new_target: Value::Undef, home }) }),
                    ));
                    let saved = self.env;
                    self.env = env;
                    let r = self.eval(e);
                    self.env = saved;
                    r?
                }
                None => Value::Undef,
            };
            self.define(this, k, v);
        }
        Ok(())
    }

    /// Calls a closure; also returns its scope (constructors read `this` from it).
    fn invoke(&mut self, fg: Gc, c: &Closure, this: Value, args: Vec<Value>, new_target: Value) -> R<(Value, Gc)> {
        let def = c.def.clone();
        let fun = if def.arrow {
            None
        } else {
            let this = if c.ctor == CtorKind::Derived {
                None
            } else {
                Some(match this {
                    // Sloppy-mode functions see the global object for undefined `this`.
                    Value::Undef | Value::Null if c.ctor == CtorKind::Normal => Value::Obj(self.global),
                    t => t,
                })
            };
            Some(FnScope { this, func: Some(fg), new_target, home: c.home })
        };
        let mut vars: HashMap<JsStr, Binding> = HashMap::with_capacity(def.params.len() + def.vars.len() + 1);
        if let (Some(n), false) = (&def.name, def.arrow) {
            vars.insert(n.clone(), Binding { v: Value::Obj(fg), mutable: true });
        }
        let simple = def.simple;
        if simple {
            for (i, (p, _)) in def.params.iter().enumerate() {
                if let Pat::Id(n) = p {
                    vars.insert(n.clone(), Binding { v: args.get(i).cloned().unwrap_or(Value::Undef), mutable: true });
                }
            }
        }
        if def.uses_args && !def.arrow {
            let a = self.new_array(args.clone());
            vars.insert("arguments".into(), Binding { v: a, mutable: true });
        }
        let env = self.alloc(Obj::new(None, Kind::Env(Env { vars, parent: Some(c.env), fun })));
        let saved = self.env;
        self.env = env;
        let r = self.invoke_body(&def, args, simple);
        self.env = saved;
        let r = if def.is_async {
            match r {
                Ok(v) => {
                    let p = super::builtins::promise_resolved(self, v)?;
                    Ok(p)
                }
                Err(Ctrl::Throw(e)) => Ok(super::builtins::promise_rejected(self, e)),
                Err(e) => Err(e),
            }
        } else {
            r
        };
        r.map(|v| (v, env))
    }

    fn invoke_body(&mut self, def: &Func, args: Vec<Value>, simple: bool) -> R<Value> {
        if !simple {
            for (i, (p, d)) in def.params.iter().enumerate() {
                let mut v = args.get(i).cloned().unwrap_or(Value::Undef);
                if matches!(v, Value::Undef) {
                    if let Some(d) = d {
                        v = self.eval(d)?;
                    }
                }
                self.bind(p, v, Some(VarKind::Let))?;
            }
        }
        if let Some(r) = &def.rest {
            let rest: Vec<Value> = args.iter().skip(def.params.len()).cloned().collect();
            let a = self.new_array(rest);
            self.bind(r, a, Some(VarKind::Let))?;
        }
        for v in &def.vars {
            self.declare_if_absent(v);
        }
        match &def.body {
            Body::Expr(e) => self.eval(e),
            Body::Block(stmts) => {
                self.hoist_functions(stmts)?;
                for s in stmts {
                    match self.exec(s) {
                        Ok(()) => {}
                        Err(Ctrl::Return(v)) => return Ok(v),
                        Err(Ctrl::Break(_)) | Err(Ctrl::Continue(_)) => return Ok(Value::Undef),
                        Err(e) => return Err(e),
                    }
                }
                Ok(Value::Undef)
            }
        }
    }

    // ---- scopes -----------------------------------------------------------------------

    fn env_mut(&mut self, g: Gc) -> &mut Env {
        match &mut self.heap.get_mut(g).kind {
            Kind::Env(e) => e,
            _ => unreachable!("not a scope"),
        }
    }

    fn env_ref(&self, g: Gc) -> &Env {
        match &self.heap.get(g).kind {
            Kind::Env(e) => e,
            _ => unreachable!("not a scope"),
        }
    }

    pub fn new_scope(&mut self) -> Gc {
        let parent = self.env;
        self.alloc(Obj::new(None, Kind::Env(Env { vars: HashMap::new(), parent: Some(parent), fun: None })))
    }

    fn declare_if_absent(&mut self, n: &Name) {
        let env = self.env;
        if env == self.genv {
            let g = self.global;
            if self.heap.get(g).props.get(n).is_none() {
                self.heap.get_mut(g).props.set(n.clone(), Slot::Data(Value::Undef, F_WRITE | F_ENUM));
            }
            return;
        }
        let e = self.env_mut(env);
        e.vars.entry(n.clone()).or_insert(Binding { v: Value::Undef, mutable: true });
    }

    /// Declares a binding in the current scope (`var` goes to the function scope).
    fn declare(&mut self, n: &Name, v: Value, kind: VarKind) {
        let mut env = self.env;
        if kind == VarKind::Var {
            // The nearest function scope.
            while self.env_ref(env).fun.is_none() {
                match self.env_ref(env).parent {
                    Some(p) => env = p,
                    None => break,
                }
            }
        }
        if env == self.genv && kind == VarKind::Var {
            let g = self.global;
            self.define(g, n, v);
            return;
        }
        let mutable = kind != VarKind::Const;
        self.env_mut(env).vars.insert(n.clone(), Binding { v, mutable });
    }

    pub fn lookup(&mut self, n: &str) -> R<Value> {
        let mut e = Some(self.env);
        while let Some(g) = e {
            let env = self.env_ref(g);
            if let Some(b) = env.vars.get(n) {
                return Ok(b.v.clone());
            }
            e = env.parent;
        }
        let gl = self.global;
        if self.has_property(gl, n)? {
            return self.get_from(gl, n, &Value::Obj(gl));
        }
        Err(self.err(ErrKind::Reference, &format!("{n} is not defined")))
    }

    fn assign_var(&mut self, n: &str, v: Value) -> R<()> {
        let mut e = Some(self.env);
        while let Some(g) = e {
            let env = self.env_mut(g);
            if let Some(b) = env.vars.get_mut(n) {
                if !b.mutable {
                    return self.type_err("Assignment to constant variable.");
                }
                b.v = v;
                return Ok(());
            }
            e = env.parent;
        }
        let gl = Value::Obj(self.global);
        self.put(&gl, n, v)
    }

    fn this_value(&mut self) -> R<Value> {
        let mut e = Some(self.env);
        while let Some(g) = e {
            let env = self.env_ref(g);
            if let Some(f) = &env.fun {
                return match &f.this {
                    Some(t) => Ok(t.clone()),
                    None => Err(self.err(ErrKind::Reference, "Must call super constructor before accessing 'this'")),
                };
            }
            e = env.parent;
        }
        Ok(Value::Obj(self.global))
    }

    /// The nearest non-arrow function scope.
    fn fn_scope(&self) -> Option<Gc> {
        let mut e = Some(self.env);
        while let Some(g) = e {
            let env = self.env_ref(g);
            if env.fun.is_some() {
                return Some(g);
            }
            e = env.parent;
        }
        None
    }

    fn hoist_functions(&mut self, stmts: &[Stmt]) -> R<()> {
        for s in stmts {
            if let Stmt::Func(f) = s {
                let fv = self.make_closure(f, CtorKind::Normal, None);
                let n = f.name.clone().unwrap_or_else(|| "".into());
                if self.env == self.genv {
                    let g = self.global;
                    self.define(g, &n, fv);
                } else {
                    let env = self.env;
                    self.env_mut(env).vars.insert(n, Binding { v: fv, mutable: true });
                }
            }
        }
        Ok(())
    }

    pub fn make_closure(&mut self, f: &Rc<Func>, kind: CtorKind, home: Option<Gc>) -> Value {
        let kind = if f.arrow || f.method || f.is_async { if kind == CtorKind::Normal { CtorKind::None } else { kind } } else { kind };
        let p = self.p.function;
        let mut o = Obj::new(p, Kind::Func(Closure { def: f.clone(), env: self.env, home, ctor: kind, fields: None, field_env: None }));
        o.props.set("name".into(), Slot::Data(Value::Str(f.name.clone().unwrap_or_else(|| "".into())), F_CONF));
        o.props.set("length".into(), Slot::Data(Value::Num(f.params.iter().take_while(|(_, d)| d.is_none()).count() as f64), F_CONF));
        Value::Obj(self.alloc(o))
    }

    // ---- patterns -----------------------------------------------------------------------

    /// Binds a pattern: declares (`decl` Some) or assigns (None).
    pub fn bind(&mut self, p: &Pat, v: Value, decl: Option<VarKind>) -> R<()> {
        match p {
            Pat::Id(n) => match decl {
                Some(k) => {
                    self.declare(n, v, k);
                    Ok(())
                }
                None => self.assign_var(n, v),
            },
            Pat::Expr(e) => match &**e {
                Expr::Member(o, k, _) => {
                    let base = self.eval(o)?;
                    self.put(&base, k, v)
                }
                Expr::Index(o, k, _) => {
                    let base = self.eval(o)?;
                    let key = self.eval(k)?;
                    self.put_v(&base, &key, v)
                }
                _ => self.type_err("invalid assignment target"),
            },
            Pat::Obj(props, rest) => {
                if v.is_nullish() {
                    return self.type_err(&format!("Cannot destructure '{}' as it is {}.", if matches!(v, Value::Null) { "null" } else { "undefined" }, if matches!(v, Value::Null) { "null" } else { "undefined" }));
                }
                let mut used = Vec::new();
                for (k, sub, d) in props {
                    let key: JsStr = match k {
                        PropName::Lit(n) => n.clone(),
                        PropName::Computed(e) => {
                            let kv = self.eval(e)?;
                            self.to_key(&kv)?
                        }
                    };
                    let mut x = self.get(&v, &key)?;
                    if matches!(x, Value::Undef) {
                        if let Some(d) = d {
                            x = self.eval(d)?;
                        }
                    }
                    used.push(key);
                    self.bind(sub, x, decl)?;
                }
                if let Some(r) = rest {
                    let o = self.new_object();
                    if let Value::Obj(src) = &v {
                        for k in self.own_keys(*src, true)? {
                            if !used.contains(&k) {
                                let x = self.get(&v, &k)?;
                                self.define(o, &k, x);
                            }
                        }
                    }
                    self.bind(r, Value::Obj(o), decl)?;
                }
                Ok(())
            }
            Pat::Arr(items, rest) => {
                let mut it = self.iter_start(&v)?;
                for item in items {
                    let x = self.iter_next(&mut it)?.unwrap_or(Value::Undef);
                    if let Some((sub, d)) = item {
                        let x = if matches!(x, Value::Undef) {
                            match d {
                                Some(d) => self.eval(d)?,
                                None => x,
                            }
                        } else {
                            x
                        };
                        self.bind(sub, x, decl)?;
                    }
                }
                if let Some(r) = rest {
                    let mut all = Vec::new();
                    while let Some(x) = self.iter_next(&mut it)? {
                        all.push(x);
                        if all.len() > MAX_ARRAY {
                            return Err(self.err(ErrKind::Range, "Invalid array length"));
                        }
                    }
                    let a = self.new_array(all);
                    self.bind(r, a, decl)?;
                }
                Ok(())
            }
        }
    }

    // ---- iteration ---------------------------------------------------------------------------

    pub fn iter_start(&mut self, v: &Value) -> R<Iter> {
        match v {
            Value::Str(s) => return Ok(Iter::Str(s.chars().collect(), 0)),
            Value::Obj(g) => {
                if let Kind::Array(_) = self.heap.get(*g).kind {
                    // Arrays whose iterator wasn't replaced: walk them directly.
                    let f = self.get(v, "@@iterator")?;
                    if let Value::Obj(fg) = &f {
                        if matches!(&self.heap.get(*fg).kind, Kind::Native(n) if n.f as *const () as usize == super::builtins::array_values as *const () as usize) {
                            return Ok(Iter::Arr(*g, 0));
                        }
                    }
                }
            }
            _ => {}
        }
        let f = if v.is_nullish() { Value::Undef } else { self.get(v, "@@iterator")? };
        if !self.is_callable(&f) {
            return self.type_err(&format!("{} is not iterable", self.describe(v)));
        }
        let it = self.call(&f, v.clone(), Vec::new())?;
        let next = self.get(&it, "next")?;
        Ok(Iter::Proto(it, next))
    }

    pub fn iter_next(&mut self, it: &mut Iter) -> R<Option<Value>> {
        self.tick()?;
        match it {
            Iter::Arr(g, i) => {
                let v = match &self.heap.get(*g).kind {
                    Kind::Array(v) => v.get(*i).cloned(),
                    _ => None,
                };
                *i += 1;
                Ok(v)
            }
            Iter::Str(chars, i) => {
                let v = chars.get(*i).map(|c| Value::Str(c.to_string().into()));
                *i += 1;
                Ok(v)
            }
            Iter::Proto(o, next) => {
                let (o, next) = (o.clone(), next.clone());
                let r = self.call(&next, o, Vec::new())?;
                if !matches!(r, Value::Obj(_)) {
                    return self.type_err("Iterator result is not an object");
                }
                if Self::truthy(&self.get(&r, "done")?) {
                    return Ok(None);
                }
                Ok(Some(self.get(&r, "value")?))
            }
        }
    }

    /// All values of an iterable (spread, Array.from), bounded.
    pub fn collect(&mut self, v: &Value) -> R<Vec<Value>> {
        let mut it = self.iter_start(v)?;
        let mut out = Vec::new();
        while let Some(x) = self.iter_next(&mut it)? {
            out.push(x);
            if out.len() > MAX_ARRAY {
                return Err(self.err(ErrKind::Range, "Invalid array length"));
            }
        }
        Ok(out)
    }

    // ---- programs and statements ---------------------------------------------------------------

    pub fn run_program(&mut self, prog: &Program) -> R<()> {
        self.env = self.genv;
        for v in &prog.vars {
            self.declare_if_absent(v);
        }
        self.hoist_functions(&prog.body)?;
        for s in &prog.body {
            self.exec(s)?;
        }
        Ok(())
    }

    fn exec_block(&mut self, b: &Block) -> R<()> {
        if !b.scoped {
            for s in &b.body {
                self.exec(s)?;
            }
            return Ok(());
        }
        let saved = self.env;
        self.env = self.new_scope();
        let r = (|| {
            self.hoist_functions(&b.body)?;
            for s in &b.body {
                self.exec(s)?;
            }
            Ok(())
        })();
        self.env = saved;
        r
    }

    pub fn exec(&mut self, s: &Stmt) -> R<()> {
        self.exec_labeled(s, &[])
    }

    fn label_matches(l: &Option<Name>, labels: &[Name]) -> bool {
        match l {
            None => true,
            Some(n) => labels.contains(n),
        }
    }

    /// Runs a loop body; Ok(false) means break out of the loop.
    fn loop_body(&mut self, body: &Stmt, labels: &[Name]) -> R<bool> {
        match self.exec(body) {
            Ok(()) => Ok(true),
            Err(Ctrl::Break(l)) if Self::label_matches(&l, labels) => Ok(false),
            Err(Ctrl::Continue(l)) if Self::label_matches(&l, labels) => Ok(true),
            Err(e) => Err(e),
        }
    }

    fn exec_labeled(&mut self, s: &Stmt, labels: &[Name]) -> R<()> {
        self.tick()?;
        match s {
            Stmt::Empty | Stmt::Func(_) => Ok(()),
            Stmt::Expr(e) => {
                self.eval(e)?;
                Ok(())
            }
            Stmt::Var(kind, decls) => {
                for (p, init) in decls {
                    let v = match init {
                        Some(e) => {
                            let v = self.eval(e)?;
                            self.name_anonymous(&v, p);
                            v
                        }
                        None if *kind == VarKind::Var => {
                            // `var x;` doesn't reset an existing x.
                            if let Pat::Id(n) = p {
                                self.declare_if_absent_var(n);
                            }
                            continue;
                        }
                        None => Value::Undef,
                    };
                    self.bind(p, v, Some(*kind))?;
                }
                Ok(())
            }
            Stmt::Class(c) => {
                let v = self.eval_class(c)?;
                let n = c.name.clone().unwrap_or_else(|| "".into());
                self.declare(&n, v, VarKind::Let);
                Ok(())
            }
            Stmt::If(c, a, b) => {
                let cv = self.eval(c)?;
                if Self::truthy(&cv) {
                    self.exec(a)
                } else if let Some(b) = b {
                    self.exec(b)
                } else {
                    Ok(())
                }
            }
            Stmt::Block(b) => self.exec_block(b),
            Stmt::While(c, body) => {
                loop {
                    let cv = self.eval(c)?;
                    if !Self::truthy(&cv) || !self.loop_body(body, labels)? {
                        break;
                    }
                }
                Ok(())
            }
            Stmt::DoWhile(body, c) => {
                loop {
                    if !self.loop_body(body, labels)? {
                        break;
                    }
                    let cv = self.eval(c)?;
                    if !Self::truthy(&cv) {
                        break;
                    }
                }
                Ok(())
            }
            Stmt::For(init, test, update, body) => {
                let saved = self.env;
                let r = self.exec_for(init, test, update, body, labels);
                self.env = saved;
                r
            }
            Stmt::ForIn(head, obj, body) => {
                let ov = self.eval(obj)?;
                let keys = self.for_in_keys(&ov)?;
                let saved = self.env;
                let r = (|| {
                    for k in keys {
                        // Skip keys deleted during the loop.
                        if let Value::Obj(g) = &ov {
                            if !self.has_property(*g, &k)? {
                                continue;
                            }
                        }
                        self.env = saved;
                        self.bind_head(head, Value::Str(k))?;
                        if !self.loop_body(body, labels)? {
                            break;
                        }
                    }
                    Ok(())
                })();
                self.env = saved;
                r
            }
            Stmt::ForOf(head, obj, body) => {
                let ov = self.eval(obj)?;
                let mut it = self.iter_start(&ov)?;
                let saved = self.env;
                let r = (|| {
                    while let Some(v) = self.iter_next(&mut it)? {
                        self.env = saved;
                        self.bind_head(head, v)?;
                        if !self.loop_body(body, labels)? {
                            break;
                        }
                    }
                    Ok(())
                })();
                self.env = saved;
                r
            }
            Stmt::Return(e) => {
                let v = match e {
                    Some(e) => self.eval(e)?,
                    None => Value::Undef,
                };
                Err(Ctrl::Return(v))
            }
            Stmt::Break(l) => Err(Ctrl::Break(l.clone())),
            Stmt::Continue(l) => Err(Ctrl::Continue(l.clone())),
            Stmt::Throw(e) => {
                let v = self.eval(e)?;
                Err(Ctrl::Throw(v))
            }
            Stmt::Try(b, catch, fin) => {
                let saved = self.env;
                let mut r = self.exec_block(b);
                self.env = saved;
                if let (Err(Ctrl::Throw(v)), Some((param, cb))) = (&r, catch) {
                    let v = v.clone();
                    self.env = self.new_scope();
                    r = (|| {
                        if let Some(p) = param {
                            self.bind(p, v, Some(VarKind::Let))?;
                        }
                        self.exec_block(cb)
                    })();
                    self.env = saved;
                }
                if let Some(f) = fin {
                    if matches!(r, Err(Ctrl::Abort(_))) {
                        return r;
                    }
                    let fr = self.exec_block(f);
                    self.env = saved;
                    fr?;
                }
                r
            }
            Stmt::Switch(d, cases) => {
                let dv = self.eval(d)?;
                let saved = self.env;
                if cases.iter().any(|(_, b)| b.iter().any(|s| matches!(s, Stmt::Var(VarKind::Let | VarKind::Const, _) | Stmt::Class(_) | Stmt::Func(_)))) {
                    self.env = self.new_scope();
                }
                let r = (|| {
                    let mut start = None;
                    for (i, (t, _)) in cases.iter().enumerate() {
                        if let Some(t) = t {
                            let tv = self.eval(t)?;
                            if Self::strict_eq(&dv, &tv) {
                                start = Some(i);
                                break;
                            }
                        }
                    }
                    if start.is_none() {
                        start = cases.iter().position(|(t, _)| t.is_none());
                    }
                    let Some(start) = start else { return Ok(()) };
                    for (_, body) in &cases[start..] {
                        for s in body {
                            match self.exec(s) {
                                Ok(()) => {}
                                Err(Ctrl::Break(None)) => return Ok(()),
                                Err(e) => return Err(e),
                            }
                        }
                    }
                    Ok(())
                })();
                self.env = saved;
                r
            }
            Stmt::Labeled(l, inner) => {
                let mut ls: Vec<Name> = labels.to_vec();
                ls.push(l.clone());
                match self.exec_labeled(inner, &ls) {
                    Err(Ctrl::Break(Some(b))) if &b == l => Ok(()),
                    r => r,
                }
            }
        }
    }

    fn declare_if_absent_var(&mut self, n: &Name) {
        let saved = self.env;
        let mut env = self.env;
        while self.env_ref(env).fun.is_none() {
            match self.env_ref(env).parent {
                Some(p) => env = p,
                None => break,
            }
        }
        self.env = env;
        self.declare_if_absent(n);
        self.env = saved;
    }

    fn bind_head(&mut self, head: &ForHead, v: Value) -> R<()> {
        match head {
            ForHead::Decl(VarKind::Var, p) => self.bind(p, v, Some(VarKind::Var)),
            ForHead::Decl(k, p) => {
                self.env = self.new_scope();
                self.bind(p, v, Some(*k))
            }
            ForHead::Pat(p) => self.bind(p, v, None),
        }
    }

    fn for_in_keys(&mut self, v: &Value) -> R<Vec<JsStr>> {
        let mut out: Vec<JsStr> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut cur = match v {
            Value::Obj(g) => Some(*g),
            Value::Str(s) => {
                return Ok((0..str_len(s)).map(|i| JsStr::from(i.to_string())).collect());
            }
            _ => None,
        };
        let mut hops = 0;
        while let Some(g) = cur {
            hops += 1;
            if hops > 1000 {
                break;
            }
            for k in self.own_keys(g, false)? {
                let enumerable = match self.heap.get(g).props.get(&k) {
                    Some(s) => s.flags() & F_ENUM != 0,
                    None => true,
                };
                if seen.insert(k.clone()) && enumerable {
                    out.push(k);
                }
            }
            cur = self.heap.get(g).proto;
        }
        Ok(out)
    }

    fn exec_for(&mut self, init: &Option<Box<Stmt>>, test: &Option<Expr>, update: &Option<Expr>, body: &Stmt, labels: &[Name]) -> R<()> {
        let outer = self.env;
        let mut per_iter = false;
        if let Some(i) = init {
            if let Stmt::Var(k, _) = &**i {
                if *k != VarKind::Var {
                    self.env = self.new_scope();
                    per_iter = true;
                }
            }
            self.exec(i)?;
        }
        loop {
            if let Some(t) = test {
                let tv = self.eval(t)?;
                if !Self::truthy(&tv) {
                    break;
                }
            }
            if !self.loop_body(body, labels)? {
                break;
            }
            if per_iter {
                // A fresh copy of the loop variables, so closures made in the
                // body each keep their own iteration's values.
                let vars = self.env_ref(self.env).vars.clone();
                self.env = self.alloc(Obj::new(None, Kind::Env(Env { vars, parent: Some(outer), fun: None })));
            }
            if let Some(u) = update {
                self.eval(u)?;
            }
        }
        Ok(())
    }

    fn name_anonymous(&mut self, v: &Value, p: &Pat) {
        if let (Value::Obj(g), Pat::Id(n)) = (v, p) {
            let o = self.heap.get_mut(*g);
            if o.is_callable() {
                if let Some(Slot::Data(Value::Str(s), _)) = o.props.get("name") {
                    if s.is_empty() {
                        o.props.set("name".into(), Slot::Data(Value::Str(n.clone()), F_CONF));
                    }
                }
            }
        }
    }

    // ---- classes ----------------------------------------------------------------------------

    pub fn eval_class(&mut self, c: &Class) -> R<Value> {
        let (proto_parent, ctor_parent) = match &c.extends {
            None => (self.p.object, self.p.function),
            Some(e) => {
                let pv = self.eval(e)?;
                match &pv {
                    Value::Null => (None, self.p.function),
                    Value::Obj(g) if self.heap.get(*g).is_callable() => {
                        let pp = self.get(&pv, "prototype")?;
                        match pp {
                            Value::Obj(p) => (Some(p), Some(*g)),
                            Value::Null => (None, Some(*g)),
                            _ => return self.type_err("Class extends value does not have valid prototype property"),
                        }
                    }
                    _ => return self.type_err(&format!("Class extends value {} is not a constructor or null", self.describe(&pv))),
                }
            }
        };
        let saved = self.env;
        self.env = self.new_scope();
        let r = self.eval_class_inner(c, proto_parent, ctor_parent);
        self.env = saved;
        r
    }

    fn eval_class_inner(&mut self, c: &Class, proto_parent: Option<Gc>, ctor_parent: Option<Gc>) -> R<Value> {
        let derived = c.extends.is_some();
        let proto = self.alloc(Obj::new(proto_parent, Kind::Plain));
        let def = match &c.ctor {
            Some(f) => f.clone(),
            None => Rc::new(Func {
                name: c.name.clone(),
                params: Vec::new(),
                rest: if derived { Some(Pat::Id("args".into())) } else { None },
                body: Body::Block(if derived { vec![Stmt::Expr(Expr::SuperCall(vec![ArrEl::Spread(Expr::Ident("args".into()))]))] } else { Vec::new() }),
                arrow: false,
                is_async: false,
                method: true,
                uses_args: false,
                vars: Vec::new(),
                simple: !derived,
                line: 0,
            }),
        };
        let def = if def.name.is_none() || def.name.as_deref() == Some("constructor") {
            let mut d = (*def).clone();
            d.name = c.name.clone();
            Rc::new(d)
        } else {
            def
        };
        let class_env = self.env;
        let mut o = Obj::new(
            ctor_parent,
            Kind::Func(Closure {
                def: def.clone(),
                env: class_env,
                home: Some(proto),
                ctor: if derived { CtorKind::Derived } else { CtorKind::Base },
                fields: None,
                field_env: Some(class_env),
            }),
        );
        o.props.set("name".into(), Slot::Data(Value::Str(c.name.clone().unwrap_or_else(|| "".into())), F_CONF));
        o.props.set("length".into(), Slot::Data(Value::Num(def.params.len() as f64), F_CONF));
        o.props.set("prototype".into(), Slot::Data(Value::Obj(proto), 0));
        let ctor = self.alloc(o);
        self.hide(proto, "constructor", Value::Obj(ctor));
        if let Some(n) = &c.name {
            self.env_mut(class_env).vars.insert(n.clone(), Binding { v: Value::Obj(ctor), mutable: false });
        }
        let mut fields = Vec::new();
        let mut static_fields = Vec::new();
        for m in &c.members {
            let key: JsStr = match &m.key {
                PropName::Lit(n) => n.clone(),
                PropName::Computed(e) => {
                    let kv = self.eval(e)?;
                    self.to_key(&kv)?
                }
            };
            let target = if m.is_static { ctor } else { proto };
            match &m.kind {
                MemberKind::Method(f) => {
                    let fv = self.make_closure(f, CtorKind::None, Some(target));
                    self.hide(target, &key, fv);
                }
                MemberKind::Get(f) | MemberKind::Set(f) => {
                    let fv = self.make_closure(f, CtorKind::None, Some(target)).as_obj();
                    let is_get = matches!(m.kind, MemberKind::Get(_));
                    let o = self.heap.get_mut(target);
                    let (mut g, mut s) = match o.props.get(&key) {
                        Some(Slot::Accessor(g, s, _)) => (*g, *s),
                        _ => (None, None),
                    };
                    if is_get {
                        g = fv;
                    } else {
                        s = fv;
                    }
                    o.props.set(key.clone(), Slot::Accessor(g, s, F_CONF));
                }
                MemberKind::Field(init) => {
                    if m.is_static {
                        static_fields.push((key, init.clone()));
                    } else {
                        fields.push((key, init.clone()));
                    }
                }
            }
        }
        if !fields.is_empty() {
            if let Kind::Func(cl) = &mut self.heap.get_mut(ctor).kind {
                cl.fields = Some(Rc::new(fields));
            }
        }
        for (k, init) in static_fields {
            let v = match init {
                Some(e) => {
                    let env = self.alloc(Obj::new(
                        None,
                        Kind::Env(Env { vars: HashMap::new(), parent: Some(class_env), fun: Some(FnScope { this: Some(Value::Obj(ctor)), func: Some(ctor), new_target: Value::Undef, home: Some(ctor) }) }),
                    ));
                    let saved = self.env;
                    self.env = env;
                    let r = self.eval(&e);
                    self.env = saved;
                    r?
                }
                None => Value::Undef,
            };
            self.define(ctor, &k, v);
        }
        Ok(Value::Obj(ctor))
    }

    // ---- expressions ------------------------------------------------------------------------------

    pub fn eval(&mut self, e: &Expr) -> R<Value> {
        self.tick()?;
        self.enter()?;
        let r = self.eval_inner(e);
        self.depth -= 1;
        r
    }

    fn eval_args(&mut self, args: &[ArrEl]) -> R<Vec<Value>> {
        let mut out = Vec::with_capacity(args.len());
        for a in args {
            match a {
                ArrEl::Item(e) => out.push(self.eval(e)?),
                ArrEl::Spread(e) => {
                    let v = self.eval(e)?;
                    let all = self.collect(&v)?;
                    out.extend(all);
                    if out.len() > 100_000 {
                        return Err(self.err(ErrKind::Range, "Maximum call stack size exceeded"));
                    }
                }
                ArrEl::Hole => out.push(Value::Undef),
            }
        }
        Ok(out)
    }

    fn expr_name(e: &Expr) -> String {
        match e {
            Expr::Ident(n) => n.to_string(),
            Expr::Member(o, n, _) => format!("{}.{n}", Self::expr_name(o)),
            Expr::Index(o, _, _) => format!("{}[...]", Self::expr_name(o)),
            Expr::This => "this".into(),
            Expr::Call(c, _, _) => format!("{}(...)", Self::expr_name(c)),
            Expr::OptChain(e) => Self::expr_name(e),
            _ => "expression".into(),
        }
    }

    fn eval_ref(&mut self, e: &Expr) -> R<Ref> {
        match e {
            Expr::Ident(n) => Ok(Ref::Var(n.clone())),
            Expr::Member(o, n, _) => {
                let base = self.eval(o)?;
                Ok(Ref::Prop(base, n.clone()))
            }
            Expr::Index(o, k, _) => {
                let base = self.eval(o)?;
                let kv = self.eval(k)?;
                let key = self.to_key(&kv)?;
                Ok(Ref::Prop(base, key))
            }
            _ => self.type_err("Invalid left-hand side in assignment"),
        }
    }

    fn ref_get(&mut self, r: &Ref) -> R<Value> {
        match r {
            Ref::Var(n) => self.lookup(n),
            Ref::Prop(b, k) => self.get(b, k),
        }
    }

    fn ref_put(&mut self, r: &Ref, v: Value) -> R<()> {
        match r {
            Ref::Var(n) => self.assign_var(n, v),
            Ref::Prop(b, k) => self.put(b, k, v),
        }
    }

    fn super_base(&mut self) -> R<Option<Gc>> {
        let Some(fs) = self.fn_scope() else { return Ok(None) };
        let home = self.env_ref(fs).fun.as_ref().and_then(|f| f.home);
        Ok(home.and_then(|h| self.heap.get(h).proto))
    }

    fn eval_inner(&mut self, e: &Expr) -> R<Value> {
        Ok(match e {
            Expr::Num(n) => Value::Num(*n),
            Expr::Str(s) => Value::Str(s.clone()),
            Expr::Bool(b) => Value::Bool(*b),
            Expr::Null => Value::Null,
            Expr::Ident(n) => {
                if &**n == "undefined" {
                    return Ok(Value::Undef);
                }
                self.lookup(n)?
            }
            Expr::This => self.this_value()?,
            Expr::Template(strs, exprs) => {
                let mut s = String::new();
                for (i, part) in strs.iter().enumerate() {
                    s.push_str(part);
                    if let Some(e) = exprs.get(i) {
                        let v = self.eval(e)?;
                        let t = self.to_str(&v)?;
                        s.push_str(&t);
                    }
                    if s.len() > MAX_STRING {
                        return Err(self.err(ErrKind::Range, "Invalid string length"));
                    }
                }
                Value::Str(s.into())
            }
            Expr::TaggedTemplate(tag, strs, exprs) => {
                let (this, f) = self.callee(tag)?;
                let cooked: Vec<Value> = strs.0.iter().map(|s| Value::Str(s.clone())).collect();
                let raw: Vec<Value> = strs.1.iter().map(|s| Value::Str(s.clone())).collect();
                let arr = self.new_array(cooked);
                let rawv = self.new_array(raw);
                if let Value::Obj(g) = arr {
                    self.hide(g, "raw", rawv);
                }
                let mut args = vec![arr];
                for e in exprs {
                    args.push(self.eval(e)?);
                }
                self.call(&f, this, args)?
            }
            Expr::Regex(src, flags) => {
                let re = self.regex(src, flags)?;
                self.new_regexp(re)
            }
            Expr::Array(items) => {
                let v = self.eval_args(items)?;
                self.new_array(v)
            }
            Expr::Object(props) => self.eval_object(props)?,
            Expr::Func(f) => self.make_closure(f, CtorKind::Normal, None),
            Expr::Class(c) => self.eval_class(c)?,
            Expr::Unary(op, x) => self.eval_unary(*op, x)?,
            Expr::Update(inc, prefix, target) => {
                let r = self.eval_ref(target)?;
                let old = self.ref_get(&r)?;
                let n = self.to_num(&old)?;
                let new = if *inc { n + 1.0 } else { n - 1.0 };
                self.ref_put(&r, Value::Num(new))?;
                Value::Num(if *prefix { new } else { n })
            }
            Expr::Bin(op, a, b) => {
                let av = self.eval(a)?;
                let bv = self.eval(b)?;
                self.binop(*op, &av, &bv)?
            }
            Expr::Logic(op, a, b) => {
                let av = self.eval(a)?;
                let take_left = match op {
                    LogicOp::And => !Self::truthy(&av),
                    LogicOp::Or => Self::truthy(&av),
                    LogicOp::Nullish => !av.is_nullish(),
                };
                if take_left {
                    av
                } else {
                    self.eval(b)?
                }
            }
            Expr::Cond(c, a, b) => {
                let cv = self.eval(c)?;
                if Self::truthy(&cv) {
                    self.eval(a)?
                } else {
                    self.eval(b)?
                }
            }
            Expr::Assign(op, target, value) => self.eval_assign(*op, target, value)?,
            Expr::Seq(v) => {
                let mut last = Value::Undef;
                for x in v {
                    last = self.eval(x)?;
                }
                last
            }
            Expr::Member(o, n, opt) => {
                let base = self.eval(o)?;
                if *opt && base.is_nullish() {
                    return Err(Ctrl::Short);
                }
                self.get(&base, n)?
            }
            Expr::Index(o, k, opt) => {
                let base = self.eval(o)?;
                if *opt && base.is_nullish() {
                    return Err(Ctrl::Short);
                }
                let kv = self.eval(k)?;
                self.get_v(&base, &kv)?
            }
            Expr::OptChain(inner) => match self.eval(inner) {
                Err(Ctrl::Short) => Value::Undef,
                r => r?,
            },
            Expr::Call(callee, args, opt) => {
                let (this, f) = self.callee(callee)?;
                if *opt && f.is_nullish() {
                    return Err(Ctrl::Short);
                }
                let argv = self.eval_args(args)?;
                if !self.is_callable(&f) {
                    return self.type_err(&format!("{} is not a function", Self::expr_name(callee)));
                }
                self.call(&f, this, argv)?
            }
            Expr::New(callee, args) => {
                let f = self.eval(callee)?;
                let argv = self.eval_args(args)?;
                if !matches!(&f, Value::Obj(g) if self.heap.get(*g).is_callable()) {
                    return self.type_err(&format!("{} is not a constructor", Self::expr_name(callee)));
                }
                self.construct(&f, argv, None)?
            }
            Expr::SuperCall(args) => {
                let argv = self.eval_args(args)?;
                let Some(fs) = self.fn_scope() else { return self.type_err("'super' keyword unexpected here") };
                let (func, nt) = match &self.env_ref(fs).fun {
                    Some(f) => (f.func, f.new_target.clone()),
                    None => (None, Value::Undef),
                };
                let Some(func) = func else { return self.type_err("'super' keyword unexpected here") };
                let Some(parent) = self.heap.get(func).proto else { return self.type_err("Super constructor null of anonymous class is not a constructor") };
                let this = self.construct(&Value::Obj(parent), argv, Some(nt))?;
                if let Some(f) = &mut self.env_mut(fs).fun {
                    if f.this.is_some() {
                        return Err(self.err(ErrKind::Reference, "Super constructor may only be called once"));
                    }
                    f.this = Some(this.clone());
                }
                if let Value::Obj(t) = &this {
                    self.init_fields(func, *t)?;
                }
                Value::Undef
            }
            Expr::SuperMember(n) => {
                let this = self.this_value()?;
                match self.super_base()? {
                    Some(b) => self.get_from(b, n, &this)?,
                    None => Value::Undef,
                }
            }
            Expr::SuperIndex(k) => {
                let this = self.this_value()?;
                let kv = self.eval(k)?;
                let key = self.to_key(&kv)?;
                match self.super_base()? {
                    Some(b) => self.get_from(b, &key, &this)?,
                    None => Value::Undef,
                }
            }
            Expr::NewTarget => match self.fn_scope() {
                Some(fs) => self.env_ref(fs).fun.as_ref().map(|f| f.new_target.clone()).unwrap_or(Value::Undef),
                None => Value::Undef,
            },
            Expr::Await(x) => {
                let v = self.eval(x)?;
                super::builtins::await_value(self, v)?
            }
        })
    }

    /// The function and `this` for a call.
    fn callee(&mut self, callee: &Expr) -> R<(Value, Value)> {
        Ok(match callee {
            Expr::Member(o, n, opt) => {
                let base = self.eval(o)?;
                if *opt && base.is_nullish() {
                    return Err(Ctrl::Short);
                }
                let f = self.get(&base, n)?;
                (base, f)
            }
            Expr::Index(o, k, opt) => {
                let base = self.eval(o)?;
                if *opt && base.is_nullish() {
                    return Err(Ctrl::Short);
                }
                let kv = self.eval(k)?;
                let f = self.get_v(&base, &kv)?;
                (base, f)
            }
            Expr::SuperMember(n) => {
                let this = self.this_value()?;
                let f = match self.super_base()? {
                    Some(b) => self.get_from(b, n, &this)?,
                    None => Value::Undef,
                };
                (this, f)
            }
            _ => (Value::Undef, self.eval(callee)?),
        })
    }

    fn eval_object(&mut self, props: &[PropDef]) -> R<Value> {
        let o = self.new_object();
        for p in props {
            match p {
                PropDef::Value(k, v) => {
                    let key = self.prop_key(k)?;
                    let val = self.eval(v)?;
                    if &*key == "__proto__" && matches!(k, PropName::Lit(_)) {
                        match val {
                            Value::Obj(pg) => self.heap.get_mut(o).proto = Some(pg),
                            Value::Null => self.heap.get_mut(o).proto = None,
                            _ => {}
                        }
                        continue;
                    }
                    if let Value::Obj(f) = &val {
                        // Methods get a name.
                        if matches!(v, Expr::Func(_)) {
                            if let Some(Slot::Data(Value::Str(s), _)) = self.heap.get(*f).props.get("name") {
                                if s.is_empty() {
                                    self.heap.get_mut(*f).props.set("name".into(), Slot::Data(Value::Str(key.clone()), F_CONF));
                                }
                            }
                        }
                    }
                    self.define(o, &key, val);
                }
                PropDef::Shorthand(n) => {
                    let v = self.lookup(n)?;
                    self.define(o, n, v);
                }
                PropDef::Spread(e) => {
                    let v = self.eval(e)?;
                    match &v {
                        Value::Obj(src) => {
                            for k in self.own_keys(*src, true)? {
                                let x = self.get(&v, &k)?;
                                self.define(o, &k, x);
                            }
                        }
                        Value::Str(s) => {
                            for (i, c) in s.chars().enumerate() {
                                self.define(o, &i.to_string(), Value::Str(c.to_string().into()));
                            }
                        }
                        _ => {}
                    }
                }
                PropDef::Get(k, f) | PropDef::Set(k, f) => {
                    let key = self.prop_key(k)?;
                    let fv = self.make_closure(f, CtorKind::None, None).as_obj();
                    let is_get = matches!(p, PropDef::Get(..));
                    let obj = self.heap.get_mut(o);
                    let (mut g, mut s) = match obj.props.get(&key) {
                        Some(Slot::Accessor(g, s, _)) => (*g, *s),
                        _ => (None, None),
                    };
                    if is_get {
                        g = fv;
                    } else {
                        s = fv;
                    }
                    obj.props.set(key, Slot::Accessor(g, s, F_ENUM | F_CONF));
                }
            }
        }
        Ok(Value::Obj(o))
    }

    fn prop_key(&mut self, k: &PropName) -> R<JsStr> {
        match k {
            PropName::Lit(n) => Ok(n.clone()),
            PropName::Computed(e) => {
                let v = self.eval(e)?;
                self.to_key(&v)
            }
        }
    }

    fn eval_unary(&mut self, op: UnOp, x: &Expr) -> R<Value> {
        Ok(match op {
            UnOp::TypeOf => {
                if let Expr::Ident(n) = x {
                    match self.lookup(n) {
                        Ok(v) => return Ok(Value::str(self.type_of(&v))),
                        Err(Ctrl::Throw(_)) => return Ok(Value::str("undefined")),
                        Err(e) => return Err(e),
                    }
                }
                let v = self.eval(x)?;
                Value::str(self.type_of(&v))
            }
            UnOp::Delete => match x {
                Expr::Member(o, n, _) => {
                    let b = self.eval(o)?;
                    Value::Bool(self.delete(&b, n)?)
                }
                Expr::Index(o, k, _) => {
                    let b = self.eval(o)?;
                    let kv = self.eval(k)?;
                    let key = self.to_key(&kv)?;
                    Value::Bool(self.delete(&b, &key)?)
                }
                Expr::OptChain(inner) => match self.eval_unary(UnOp::Delete, inner) {
                    Err(Ctrl::Short) => Value::Bool(true),
                    r => r?,
                },
                _ => {
                    self.eval(x)?;
                    Value::Bool(true)
                }
            },
            UnOp::Void => {
                self.eval(x)?;
                Value::Undef
            }
            UnOp::Not => {
                let v = self.eval(x)?;
                Value::Bool(!Self::truthy(&v))
            }
            UnOp::Neg => {
                let v = self.eval(x)?;
                Value::Num(-self.to_num(&v)?)
            }
            UnOp::Plus => {
                let v = self.eval(x)?;
                Value::Num(self.to_num(&v)?)
            }
            UnOp::BitNot => {
                let v = self.eval(x)?;
                Value::Num(!self.to_int32(&v)? as f64)
            }
        })
    }

    fn eval_assign(&mut self, op: AssignOp, target: &Pat, value: &Expr) -> R<Value> {
        match op {
            AssignOp::Set => match target {
                Pat::Id(n) => {
                    let v = self.eval(value)?;
                    self.name_anonymous(&v, target);
                    self.assign_var(n, v.clone())?;
                    Ok(v)
                }
                Pat::Expr(e) => {
                    let r = self.eval_ref(e)?;
                    let v = self.eval(value)?;
                    self.ref_put(&r, v.clone())?;
                    Ok(v)
                }
                _ => {
                    let v = self.eval(value)?;
                    self.bind(target, v.clone(), None)?;
                    Ok(v)
                }
            },
            AssignOp::Op(b) => {
                let r = match target {
                    Pat::Id(n) => Ref::Var(n.clone()),
                    Pat::Expr(e) => self.eval_ref(e)?,
                    _ => return self.type_err("Invalid left-hand side in assignment"),
                };
                let old = self.ref_get(&r)?;
                let rhs = self.eval(value)?;
                let new = self.binop(b, &old, &rhs)?;
                self.ref_put(&r, new.clone())?;
                Ok(new)
            }
            AssignOp::Logic(l) => {
                let r = match target {
                    Pat::Id(n) => Ref::Var(n.clone()),
                    Pat::Expr(e) => self.eval_ref(e)?,
                    _ => return self.type_err("Invalid left-hand side in assignment"),
                };
                let old = self.ref_get(&r)?;
                let keep = match l {
                    LogicOp::And => !Self::truthy(&old),
                    LogicOp::Or => Self::truthy(&old),
                    LogicOp::Nullish => !old.is_nullish(),
                };
                if keep {
                    return Ok(old);
                }
                let v = self.eval(value)?;
                self.ref_put(&r, v.clone())?;
                Ok(v)
            }
        }
    }

    pub fn concat(&mut self, a: &str, b: &str) -> R<Value> {
        if a.len() + b.len() > MAX_STRING {
            return Err(self.err(ErrKind::Range, "Invalid string length"));
        }
        let mut s = String::with_capacity(a.len() + b.len());
        s.push_str(a);
        s.push_str(b);
        Ok(Value::Str(s.into()))
    }

    pub fn binop(&mut self, op: BinOp, a: &Value, b: &Value) -> R<Value> {
        use BinOp::*;
        if let (Value::Num(x), Value::Num(y)) = (a, b) {
            let (x, y) = (*x, *y);
            match op {
                Add => return Ok(Value::Num(x + y)),
                Sub => return Ok(Value::Num(x - y)),
                Mul => return Ok(Value::Num(x * y)),
                Lt => return Ok(Value::Bool(x < y)),
                Gt => return Ok(Value::Bool(x > y)),
                Le => return Ok(Value::Bool(x <= y)),
                Ge => return Ok(Value::Bool(x >= y)),
                StrictEq | Eq => return Ok(Value::Bool(x == y)),
                StrictNe | Ne => return Ok(Value::Bool(x != y)),
                _ => {}
            }
        }
        Ok(match op {
            Add => {
                if let (Value::Str(x), Value::Str(y)) = (a, b) {
                    return self.concat(x, y);
                }
                let pa = self.to_primitive(a, false)?;
                let pb = self.to_primitive(b, false)?;
                if matches!(pa, Value::Str(_)) || matches!(pb, Value::Str(_)) {
                    let sa = self.to_str(&pa)?;
                    let sb = self.to_str(&pb)?;
                    return self.concat(&sa, &sb);
                }
                Value::Num(self.to_num(&pa)? + self.to_num(&pb)?)
            }
            Sub => Value::Num(self.to_num(a)? - self.to_num(b)?),
            Mul => Value::Num(self.to_num(a)? * self.to_num(b)?),
            Div => Value::Num(self.to_num(a)? / self.to_num(b)?),
            Mod => {
                let (x, y) = (self.to_num(a)?, self.to_num(b)?);
                Value::Num(x % y)
            }
            Exp => {
                let (x, y) = (self.to_num(a)?, self.to_num(b)?);
                Value::Num(if y.is_nan() || ((x == 1.0 || x == -1.0) && y.is_infinite()) { f64::NAN } else { x.powf(y) })
            }
            Shl => {
                let (x, y) = (self.to_int32(a)?, self.to_int32(b)?);
                Value::Num(x.wrapping_shl(y as u32 & 31) as f64)
            }
            Shr => {
                let (x, y) = (self.to_int32(a)?, self.to_int32(b)?);
                Value::Num((x >> (y as u32 & 31)) as f64)
            }
            UShr => {
                let (x, y) = (self.to_int32(a)? as u32, self.to_int32(b)?);
                Value::Num((x >> (y as u32 & 31)) as f64)
            }
            BitAnd => Value::Num((self.to_int32(a)? & self.to_int32(b)?) as f64),
            BitOr => Value::Num((self.to_int32(a)? | self.to_int32(b)?) as f64),
            BitXor => Value::Num((self.to_int32(a)? ^ self.to_int32(b)?) as f64),
            StrictEq => Value::Bool(Self::strict_eq(a, b)),
            StrictNe => Value::Bool(!Self::strict_eq(a, b)),
            Eq => Value::Bool(self.loose_eq(a, b)?),
            Ne => Value::Bool(!self.loose_eq(a, b)?),
            Lt | Gt | Le | Ge => {
                let pa = self.to_primitive(a, false)?;
                let pb = self.to_primitive(b, false)?;
                if let (Value::Str(x), Value::Str(y)) = (&pa, &pb) {
                    let o = x.cmp(y);
                    return Ok(Value::Bool(match op {
                        Lt => o.is_lt(),
                        Gt => o.is_gt(),
                        Le => o.is_le(),
                        _ => o.is_ge(),
                    }));
                }
                let (x, y) = (self.to_num(&pa)?, self.to_num(&pb)?);
                Value::Bool(match op {
                    Lt => x < y,
                    Gt => x > y,
                    Le => x <= y,
                    _ => x >= y,
                })
            }
            In => {
                let Value::Obj(g) = b else { return self.type_err("Cannot use 'in' operator to search for a key in a non-object") };
                let k = self.to_key(a)?;
                Value::Bool(self.has_property(*g, &k)?)
            }
            InstanceOf => Value::Bool(self.instance_of(a, b)?),
        })
    }

    pub fn instance_of(&mut self, a: &Value, b: &Value) -> R<bool> {
        let Value::Obj(f) = b else { return self.type_err("Right-hand side of 'instanceof' is not callable") };
        if !self.heap.get(*f).is_callable() {
            return self.type_err("Right-hand side of 'instanceof' is not callable");
        }
        if let Kind::Bound { target, .. } = &self.heap.get(*f).kind {
            let t = Value::Obj(*target);
            return self.instance_of(a, &t);
        }
        let Value::Obj(mut o) = a else { return Ok(false) };
        let proto = self.get(b, "prototype")?;
        let Value::Obj(p) = proto else { return self.type_err("Function has non-object prototype in instanceof check") };
        let mut hops = 0;
        while let Some(x) = self.heap.get(o).proto {
            if x == p {
                return Ok(true);
            }
            o = x;
            hops += 1;
            if hops > 10_000 {
                break;
            }
        }
        Ok(false)
    }

    // ---- the collector ----------------------------------------------------------------------------

    /// Collects garbage; only call between tasks.
    pub fn gc(&mut self) {
        let mut roots = vec![self.global, self.genv, self.env];
        let p = self.p.clone();
        roots.extend(
            [
                p.object, p.function, p.array, p.string, p.number, p.boolean, p.error, p.type_error, p.range_error, p.syntax_error, p.reference_error, p.regexp,
                p.date, p.map, p.set, p.promise, p.iter, p.node, p.event, p.class_list, p.style, p.dataset,
            ]
            .into_iter()
            .flatten(),
        );
        for (f, args) in &self.microtasks {
            roots.extend(f.as_obj());
            roots.extend(args.iter().filter_map(|a| a.as_obj()));
        }
        roots.extend(self.extra_roots.iter().filter_map(|v| v.as_obj()));
        roots.extend(self.unhandled.iter().copied());
        roots.extend(self.host.roots().iter().filter_map(|v| v.as_obj()));
        self.heap.collect(roots);
    }
}

pub enum Iter {
    Arr(Gc, usize),
    Str(Vec<char>, usize),
    Proto(Value, Value),
}

pub fn f64_to_int32(n: f64) -> i32 {
    if !n.is_finite() {
        return 0;
    }
    let t = n.trunc();
    let m = t.rem_euclid(4294967296.0);
    (m as u64 as u32) as i32
}

/// String length in characters.
pub fn str_len(s: &str) -> usize {
    if s.is_ascii() {
        s.len()
    } else {
        s.chars().count()
    }
}

pub fn str_at(s: &str, i: usize) -> Option<char> {
    if s.is_ascii() {
        s.as_bytes().get(i).map(|&b| b as char)
    } else {
        s.chars().nth(i)
    }
}
