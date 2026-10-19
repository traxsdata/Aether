//! script/builtins.rs - the standard library: Object, Function, Array,
//! String, Number, Boolean, Symbol, Math, JSON, errors, RegExp, Date, Map,
//! Set, Promise, Reflect and the global functions.
//!
//! Left out on purpose: `eval` and the `Function` constructor (code from
//! strings), Proxy, SharedArrayBuffer and Atomics, WebAssembly. Time is
//! coarse (milliseconds) and there is no time zone: dates are UTC.

use std::collections::HashMap;
use std::rc::Rc;

use super::interp::{f64_to_int32, str_at, str_len, Args, Ctrl, ErrKind, Interp, R, MAX_ARRAY, MAX_STRING};
use super::value::*;

type A<'a> = Args<'a>;

fn ctor(it: &mut Interp, name: &str, f: NativeFn, len: u32, proto: Gc) -> Gc {
    let c = it.new_native(name, f, len);
    if let Kind::Native(n) = &mut it.heap.get_mut(c).kind {
        n.ctor = true;
    }
    it.heap.get_mut(c).props.set("prototype".into(), Slot::Data(Value::Obj(proto), 0));
    it.hide(proto, "constructor", Value::Obj(c));
    let g = it.global;
    it.hide(g, name, Value::Obj(c));
    c
}

fn num(v: f64) -> Value {
    Value::Num(v)
}

pub fn install(it: &mut Interp) {
    // The two roots of everything, made before anything that needs them.
    let object = it.alloc(Obj::new(None, Kind::Plain));
    it.p.object = Some(object);
    let function = it.alloc(Obj::new(Some(object), Kind::Native(Native { f: |_, _| Ok(Value::Undef), data: Vec::new(), ctor: false })));
    it.p.function = Some(function);
    let g = it.global;
    it.heap.get_mut(g).proto = Some(object);

    for (field, kind) in [("array", Kind::Array(Vec::new())), ("string", Kind::Boxed(Value::str(""))), ("number", Kind::Boxed(Value::Num(0.0))), ("boolean", Kind::Boxed(Value::Bool(false)))] {
        let p = it.alloc(Obj::new(Some(object), kind));
        match field {
            "array" => it.p.array = Some(p),
            "string" => it.p.string = Some(p),
            "number" => it.p.number = Some(p),
            _ => it.p.boolean = Some(p),
        }
    }
    let mk = |it: &mut Interp| it.new_object();
    let error = mk(it);
    it.p.error = Some(error);
    for f in ["regexp", "date", "map", "set", "promise", "iter"] {
        let p = mk(it);
        match f {
            "regexp" => it.p.regexp = Some(p),
            "date" => it.p.date = Some(p),
            "map" => it.p.map = Some(p),
            "set" => it.p.set = Some(p),
            "promise" => it.p.promise = Some(p),
            _ => it.p.iter = Some(p),
        }
    }

    install_object(it, object);
    install_function(it, function);
    install_array(it);
    install_string(it);
    install_number(it);
    install_errors(it, error);
    install_math(it);
    install_json(it);
    install_regexp(it);
    install_date(it);
    install_collections(it);
    install_promise(it);
    install_globals(it);
}

// ---- Object --------------------------------------------------------------------

fn obj_arg(it: &mut Interp, v: &Value) -> R<Gc> {
    match v {
        Value::Obj(g) => Ok(*g),
        Value::Undef | Value::Null => it.type_err("Cannot convert undefined or null to object"),
        Value::Str(_) => {
            let p = it.p.string;
            Ok(it.alloc(Obj::new(p, Kind::Boxed(v.clone()))))
        }
        _ => {
            let p = if matches!(v, Value::Num(_)) { it.p.number } else { it.p.boolean };
            Ok(it.alloc(Obj::new(p, Kind::Boxed(v.clone()))))
        }
    }
}

fn install_object(it: &mut Interp, proto: Gc) {
    let c = ctor(
        it,
        "Object",
        |it, a| match a.get(0) {
            Value::Undef | Value::Null => Ok(Value::Obj(it.new_object())),
            v => Ok(Value::Obj(obj_arg(it, &v)?)),
        },
        1,
        proto,
    );
    it.method(proto, "hasOwnProperty", |it, a| {
        let k = it.to_key(&a.get(0))?;
        let o = obj_arg(it, &a.this)?;
        Ok(Value::Bool(it.has_own(o, &k)?))
    }, 1);
    it.method(proto, "isPrototypeOf", |it, a| {
        let (Value::Obj(p), Value::Obj(mut o)) = (a.this.clone(), a.get(0)) else { return Ok(Value::Bool(false)) };
        while let Some(x) = it.heap.get(o).proto {
            if x == p {
                return Ok(Value::Bool(true));
            }
            o = x;
        }
        Ok(Value::Bool(false))
    }, 1);
    it.method(proto, "propertyIsEnumerable", |it, a| {
        let k = it.to_key(&a.get(0))?;
        let o = obj_arg(it, &a.this)?;
        Ok(Value::Bool(match it.heap.get(o).props.get(&k) {
            Some(s) => s.flags() & F_ENUM != 0,
            None => it.has_own(o, &k)?,
        }))
    }, 1);
    it.method(proto, "toString", |it, a| {
        let tag = match &a.this {
            Value::Undef => "Undefined",
            Value::Null => "Null",
            Value::Num(_) => "Number",
            Value::Str(_) => "String",
            Value::Bool(_) => "Boolean",
            Value::Obj(g) => match &it.heap.get(*g).kind {
                Kind::Array(_) => "Array",
                Kind::Func(_) | Kind::Native(_) | Kind::Bound { .. } => "Function",
                Kind::Error => "Error",
                Kind::Date(_) => "Date",
                Kind::RegExp(_) => "RegExp",
                Kind::Map(_) => "Map",
                Kind::Set(_) => "Set",
                Kind::Promise(_) => "Promise",
                _ => "Object",
            },
        };
        Ok(Value::Str(format!("[object {tag}]").into()))
    }, 0);
    it.method(proto, "toLocaleString", |it, a| {
        let f = it.get(&a.this, "toString")?;
        it.call(&f, a.this.clone(), Vec::new())
    }, 0);
    it.method(proto, "valueOf", |it, a| Ok(Value::Obj(obj_arg(it, &a.this)?)), 0);
    let gp = it.new_native("__proto__", |it, a| match &a.this {
        Value::Obj(g) => Ok(it.heap.get(*g).proto.map(Value::Obj).unwrap_or(Value::Null)),
        v => {
            let g = obj_arg(it, v)?;
            Ok(it.heap.get(g).proto.map(Value::Obj).unwrap_or(Value::Null))
        }
    }, 0);
    let sp = it.new_native("__proto__", |it, a| {
        if let Value::Obj(g) = &a.this {
            match a.get(0) {
                Value::Obj(p) => {
                    if !creates_cycle(it, *g, p) {
                        it.heap.get_mut(*g).proto = Some(p);
                    }
                }
                Value::Null => it.heap.get_mut(*g).proto = None,
                _ => {}
            }
        }
        Ok(Value::Undef)
    }, 1);
    it.heap.get_mut(proto).props.set("__proto__".into(), Slot::Accessor(Some(gp), Some(sp), F_CONF));

    it.method(c, "keys", |it, a| {
        let o = obj_arg(it, &a.get(0))?;
        let k = it.own_keys(o, true)?;
        Ok(it.new_array(k.into_iter().map(Value::Str).collect()))
    }, 1);
    it.method(c, "values", |it, a| {
        let v = a.get(0);
        let o = obj_arg(it, &v)?;
        let ov = Value::Obj(o);
        let mut out = Vec::new();
        for k in it.own_keys(o, true)? {
            out.push(it.get(&ov, &k)?);
        }
        Ok(it.new_array(out))
    }, 1);
    it.method(c, "entries", |it, a| {
        let o = obj_arg(it, &a.get(0))?;
        let ov = Value::Obj(o);
        let mut out = Vec::new();
        for k in it.own_keys(o, true)? {
            let v = it.get(&ov, &k)?;
            let pair = it.new_array(vec![Value::Str(k), v]);
            out.push(pair);
        }
        Ok(it.new_array(out))
    }, 1);
    it.method(c, "assign", |it, a| {
        let t = obj_arg(it, &a.get(0))?;
        let tv = Value::Obj(t);
        for src in a.args.iter().skip(1) {
            if let Value::Obj(s) = src {
                for k in it.own_keys(*s, true)? {
                    let v = it.get(src, &k)?;
                    it.put(&tv, &k, v)?;
                }
            }
        }
        Ok(tv)
    }, 2);
    it.method(c, "create", |it, a| {
        let proto = match a.get(0) {
            Value::Obj(p) => Some(p),
            Value::Null => None,
            _ => return it.type_err("Object prototype may only be an Object or null"),
        };
        let o = it.alloc(Obj::new(proto, Kind::Plain));
        if let Value::Obj(props) = a.get(1) {
            define_properties(it, o, props)?;
        }
        Ok(Value::Obj(o))
    }, 2);
    fn freeze(it: &mut Interp, a: &A, writable: bool) -> R<Value> {
        if let Value::Obj(g) = a.get(0) {
            let o = it.heap.get_mut(g);
            o.extensible = false;
            let keys: Vec<JsStr> = o.props.keys().to_vec();
            for k in keys {
                if let Some(s) = o.props.get_mut(&k) {
                    match s {
                        Slot::Data(_, f) => {
                            *f &= !F_CONF;
                            if !writable {
                                *f &= !F_WRITE;
                            }
                        }
                        Slot::Accessor(_, _, f) => *f &= !F_CONF,
                    }
                }
            }
        }
        Ok(a.get(0))
    }
    it.method(c, "freeze", |it, a| freeze(it, a, false), 1);
    it.method(c, "seal", |it, a| freeze(it, a, true), 1);
    it.method(c, "preventExtensions", |it, a| {
        if let Value::Obj(g) = a.get(0) {
            it.heap.get_mut(g).extensible = false;
        }
        Ok(a.get(0))
    }, 1);
    it.method(c, "isFrozen", |it, a| {
        Ok(Value::Bool(match a.get(0) {
            Value::Obj(g) => {
                let o = it.heap.get(g);
                !o.extensible && o.props.iter().all(|(_, s)| s.flags() & (F_CONF | F_WRITE) == 0 || matches!(s, Slot::Accessor(_, _, f) if f & F_CONF == 0))
            }
            _ => true,
        }))
    }, 1);
    it.method(c, "isSealed", |it, a| {
        Ok(Value::Bool(match a.get(0) {
            Value::Obj(g) => !it.heap.get(g).extensible && it.heap.get(g).props.iter().all(|(_, s)| s.flags() & F_CONF == 0),
            _ => true,
        }))
    }, 1);
    it.method(c, "isExtensible", |it, a| Ok(Value::Bool(matches!(a.get(0), Value::Obj(g) if it.heap.get(g).extensible))), 1);
    it.method(c, "getPrototypeOf", |it, a| {
        let o = obj_arg(it, &a.get(0))?;
        Ok(it.heap.get(o).proto.map(Value::Obj).unwrap_or(Value::Null))
    }, 1);
    it.method(c, "setPrototypeOf", |it, a| {
        if let Value::Obj(g) = a.get(0) {
            match a.get(1) {
                Value::Obj(p) => {
                    if creates_cycle(it, g, p) {
                        return it.type_err("Cyclic __proto__ value");
                    }
                    it.heap.get_mut(g).proto = Some(p);
                }
                Value::Null => it.heap.get_mut(g).proto = None,
                _ => return it.type_err("Object prototype may only be an Object or null"),
            }
        }
        Ok(a.get(0))
    }, 2);
    it.method(c, "defineProperty", |it, a| {
        let Value::Obj(o) = a.get(0) else { return it.type_err("Object.defineProperty called on non-object") };
        let k = it.to_key(&a.get(1))?;
        let Value::Obj(d) = a.get(2) else { return it.type_err("Property description must be an object") };
        define_property(it, o, &k, d)?;
        Ok(a.get(0))
    }, 3);
    it.method(c, "defineProperties", |it, a| {
        let Value::Obj(o) = a.get(0) else { return it.type_err("Object.defineProperties called on non-object") };
        if let Value::Obj(p) = a.get(1) {
            define_properties(it, o, p)?;
        }
        Ok(a.get(0))
    }, 2);
    it.method(c, "getOwnPropertyNames", |it, a| {
        let o = obj_arg(it, &a.get(0))?;
        let mut k = it.own_keys(o, false)?;
        if matches!(it.heap.get(o).kind, Kind::Array(_)) {
            k.push("length".into());
        }
        Ok(it.new_array(k.into_iter().map(Value::Str).collect()))
    }, 1);
    it.method(c, "getOwnPropertySymbols", |it, _| Ok(it.new_array(Vec::new())), 1);
    it.method(c, "getOwnPropertyDescriptor", |it, a| {
        let o = obj_arg(it, &a.get(0))?;
        let k = it.to_key(&a.get(1))?;
        let slot = it.heap.get(o).props.get(&k).cloned();
        let d = it.new_object();
        match slot {
            Some(Slot::Data(v, f)) => {
                it.set_prop(d, "value", v);
                it.set_prop(d, "writable", Value::Bool(f & F_WRITE != 0));
                it.set_prop(d, "enumerable", Value::Bool(f & F_ENUM != 0));
                it.set_prop(d, "configurable", Value::Bool(f & F_CONF != 0));
            }
            Some(Slot::Accessor(g, s, f)) => {
                it.set_prop(d, "get", g.map(Value::Obj).unwrap_or(Value::Undef));
                it.set_prop(d, "set", s.map(Value::Obj).unwrap_or(Value::Undef));
                it.set_prop(d, "enumerable", Value::Bool(f & F_ENUM != 0));
                it.set_prop(d, "configurable", Value::Bool(f & F_CONF != 0));
            }
            None => {
                if !it.has_own(o, &k)? {
                    return Ok(Value::Undef);
                }
                let v = it.get(&Value::Obj(o), &k)?;
                it.set_prop(d, "value", v);
                for f in ["writable", "enumerable", "configurable"] {
                    it.set_prop(d, f, Value::Bool(true));
                }
            }
        }
        Ok(Value::Obj(d))
    }, 2);
    it.method(c, "fromEntries", |it, a| {
        let o = it.new_object();
        let items = it.collect(&a.get(0))?;
        for e in items {
            let k = it.get_v(&e, &Value::Num(0.0))?;
            let v = it.get_v(&e, &Value::Num(1.0))?;
            let k = it.to_key(&k)?;
            it.define(o, &k, v);
        }
        Ok(Value::Obj(o))
    }, 1);
    it.method(c, "is", |_, a| {
        let (x, y) = (a.get(0), a.get(1));
        Ok(Value::Bool(match (&x, &y) {
            (Value::Num(p), Value::Num(q)) => (p.is_nan() && q.is_nan()) || (p == q && p.is_sign_negative() == q.is_sign_negative()),
            _ => Interp::strict_eq(&x, &y),
        }))
    }, 2);
}

fn creates_cycle(it: &Interp, o: Gc, mut p: Gc) -> bool {
    let mut hops = 0;
    loop {
        if p == o {
            return true;
        }
        match it.heap.get(p).proto {
            Some(x) => p = x,
            None => return false,
        }
        hops += 1;
        if hops > 10_000 {
            return true;
        }
    }
}

fn define_properties(it: &mut Interp, o: Gc, props: Gc) -> R<()> {
    let pv = Value::Obj(props);
    for k in it.own_keys(props, true)? {
        if let Value::Obj(d) = it.get(&pv, &k)? {
            define_property(it, o, &k, d)?;
        }
    }
    Ok(())
}

fn define_property(it: &mut Interp, o: Gc, k: &str, d: Gc) -> R<()> {
    let dv = Value::Obj(d);
    let has = |it: &mut Interp, f: &str| it.has_property(d, f);
    let mut flags = 0u8;
    let existing = it.heap.get(o).props.get(k).cloned();
    if let Some(s) = &existing {
        if s.flags() & F_CONF == 0 {
            // Non-configurable: only a writable value may change.
            if let (Slot::Data(_, f), true) = (s, has(it, "value")?) {
                if f & F_WRITE != 0 {
                    let v = it.get(&dv, "value")?;
                    it.heap.get_mut(o).props.set(k.into(), Slot::Data(v, *f));
                    return Ok(());
                }
            }
            return it.type_err(&format!("Cannot redefine property: {k}"));
        }
        flags = s.flags() & !F_WRITE;
    }
    let flag = |it: &mut Interp, name: &str, bit: u8, flags: &mut u8| -> R<()> {
        if it.has_property(d, name)? {
            let v = it.get(&Value::Obj(d), name)?;
            if Interp::truthy(&v) {
                *flags |= bit;
            } else {
                *flags &= !bit;
            }
        }
        Ok(())
    };
    flag(it, "enumerable", F_ENUM, &mut flags)?;
    flag(it, "configurable", F_CONF, &mut flags)?;
    if has(it, "get")? || has(it, "set")? {
        let g = it.get(&dv, "get")?.as_obj();
        let s = it.get(&dv, "set")?.as_obj();
        let (og, os) = match &existing {
            Some(Slot::Accessor(a, b, _)) => (*a, *b),
            _ => (None, None),
        };
        let g = if has(it, "get")? { g } else { og };
        let s = if has(it, "set")? { s } else { os };
        it.heap.get_mut(o).props.set(k.into(), Slot::Accessor(g, s, flags));
        return Ok(());
    }
    if let Some(Slot::Data(_, f)) = &existing {
        flags |= f & F_WRITE;
    }
    flag(it, "writable", F_WRITE, &mut flags)?;
    let v = if has(it, "value")? {
        it.get(&dv, "value")?
    } else {
        match existing {
            Some(Slot::Data(v, _)) => v,
            _ => Value::Undef,
        }
    };
    if let Kind::Array(arr) = &mut it.heap.get_mut(o).kind {
        if let Some(i) = index_of_key(k) {
            if i < MAX_ARRAY {
                if i >= arr.len() {
                    arr.resize(i + 1, Value::Undef);
                }
                arr[i] = v;
            }
            return Ok(());
        }
    }
    it.heap.get_mut(o).props.set(k.into(), Slot::Data(v, flags));
    Ok(())
}

// ---- Function -------------------------------------------------------------------

fn install_function(it: &mut Interp, proto: Gc) {
    it.hide(proto, "name", Value::str(""));
    let c = ctor(it, "Function", |it, _| it.type_err("The Function constructor is disabled in this browser (no code from strings)"), 1, proto);
    let _ = c;
    it.method(proto, "call", |it, a| {
        let this = a.get(0);
        let rest = a.args.iter().skip(1).cloned().collect();
        it.call(&a.this, this, rest)
    }, 1);
    it.method(proto, "apply", |it, a| {
        let this = a.get(0);
        let args = match a.get(1) {
            Value::Undef | Value::Null => Vec::new(),
            v => array_like(it, &v)?,
        };
        it.call(&a.this, this, args)
    }, 2);
    it.method(proto, "bind", |it, a| {
        let Value::Obj(target) = a.this else { return it.type_err("Bind must be called on a function") };
        if !it.heap.get(target).is_callable() {
            return it.type_err("Bind must be called on a function");
        }
        let p = it.p.function;
        let name = match it.heap.get(target).props.get("name") {
            Some(Slot::Data(Value::Str(s), _)) => format!("bound {s}"),
            _ => "bound ".into(),
        };
        let mut o = Obj::new(p, Kind::Bound { target, this: a.get(0), args: a.args.iter().skip(1).cloned().collect() });
        o.props.set("name".into(), Slot::Data(Value::Str(name.into()), F_CONF));
        Ok(Value::Obj(it.alloc(o)))
    }, 1);
    it.method(proto, "toString", |it, a| {
        let name = match &a.this {
            Value::Obj(g) => match it.heap.get(*g).props.get("name") {
                Some(Slot::Data(Value::Str(s), _)) => s.to_string(),
                _ => String::new(),
            },
            _ => String::new(),
        };
        Ok(Value::Str(format!("function {name}() {{ [code] }}").into()))
    }, 0);
}

/// The elements of an array or array-like.
pub fn array_like(it: &mut Interp, v: &Value) -> R<Vec<Value>> {
    let Value::Obj(g) = v else {
        if let Value::Str(s) = v {
            return Ok(s.chars().map(|c| Value::Str(c.to_string().into())).collect());
        }
        return Ok(Vec::new());
    };
    if let Kind::Array(a) = &it.heap.get(*g).kind {
        return Ok(a.clone());
    }
    let len = it.get(v, "length")?;
    let n = it.to_num(&len)?;
    let n = if n.is_nan() || n < 0.0 { 0 } else { (n as usize).min(MAX_ARRAY) };
    let mut out = Vec::with_capacity(n.min(4096));
    for i in 0..n {
        out.push(it.get_v(v, &Value::Num(i as f64))?);
    }
    Ok(out)
}

// ---- Array --------------------------------------------------------------------------

fn this_array(it: &mut Interp, a: &A) -> R<Gc> {
    match &a.this {
        Value::Obj(g) if matches!(it.heap.get(*g).kind, Kind::Array(_)) => Ok(*g),
        _ => it.type_err("Array method called on a non-array"),
    }
}

fn arr_mut(it: &mut Interp, g: Gc) -> &mut Vec<Value> {
    match &mut it.heap.get_mut(g).kind {
        Kind::Array(v) => v,
        _ => unreachable!(),
    }
}

fn arr_len(it: &Interp, g: Gc) -> usize {
    match &it.heap.get(g).kind {
        Kind::Array(v) => v.len(),
        _ => 0,
    }
}

fn arr_get(it: &Interp, g: Gc, i: usize) -> Value {
    match &it.heap.get(g).kind {
        Kind::Array(v) => v.get(i).cloned().unwrap_or(Value::Undef),
        _ => Value::Undef,
    }
}

/// A relative index argument (negative counts from the end), clamped to 0..=len.
fn rel_index(it: &mut Interp, v: &Value, len: usize, default: usize) -> R<usize> {
    if matches!(v, Value::Undef) {
        return Ok(default);
    }
    let n = it.to_num(v)?;
    let n = if n.is_nan() { 0.0 } else { n.trunc() };
    Ok(if n < 0.0 { (len as f64 + n).max(0.0) as usize } else { (n as usize).min(len) })
}

fn callback(it: &mut Interp, a: &A) -> R<Value> {
    let f = a.get(0);
    if !it.is_callable(&f) {
        return it.type_err(&format!("{} is not a function", it.type_of(&f)));
    }
    Ok(f)
}

/// Elements of `this` for the read-only methods (arrays and array-likes).
fn this_elems(it: &mut Interp, a: &A) -> R<(Value, Vec<Value>)> {
    let v = a.this.clone();
    if v.is_nullish() {
        return it.type_err("Array.prototype method called on null or undefined");
    }
    let e = array_like(it, &v)?;
    Ok((v, e))
}

pub fn array_values(it: &mut Interp, a: &A) -> R<Value> {
    make_iter(it, a.this.clone(), IterKind::Values)
}

pub fn make_iter(it: &mut Interp, target: Value, kind: IterKind) -> R<Value> {
    let p = it.p.iter;
    Ok(Value::Obj(it.alloc(Obj::new(p, Kind::Iter { target, pos: 0, kind }))))
}

fn merge_sort(it: &mut Interp, v: Vec<Value>, cmp: &Value) -> R<Vec<Value>> {
    if v.len() <= 1 {
        return Ok(v);
    }
    let mut v = v;
    let right = v.split_off(v.len() / 2);
    let l = merge_sort(it, v, cmp)?;
    let r = merge_sort(it, right, cmp)?;
    let mut out = Vec::with_capacity(l.len() + r.len());
    let (mut i, mut j) = (0, 0);
    while i < l.len() && j < r.len() {
        let le = sort_le(it, &l[i], &r[j], cmp)?;
        if le {
            out.push(l[i].clone());
            i += 1;
        } else {
            out.push(r[j].clone());
            j += 1;
        }
    }
    out.extend_from_slice(&l[i..]);
    out.extend_from_slice(&r[j..]);
    Ok(out)
}

fn sort_le(it: &mut Interp, a: &Value, b: &Value, cmp: &Value) -> R<bool> {
    match (a, b) {
        (Value::Undef, Value::Undef) => return Ok(true),
        (Value::Undef, _) => return Ok(false),
        (_, Value::Undef) => return Ok(true),
        _ => {}
    }
    if matches!(cmp, Value::Undef) {
        let x = it.to_str(a)?;
        let y = it.to_str(b)?;
        return Ok(x <= y);
    }
    let r = it.call(cmp, Value::Undef, vec![a.clone(), b.clone()])?;
    let n = it.to_num(&r)?;
    Ok(!(n > 0.0))
}

fn flatten(it: &mut Interp, v: &[Value], depth: f64, out: &mut Vec<Value>) -> R<()> {
    for x in v {
        let is_arr = matches!(x, Value::Obj(g) if matches!(it.heap.get(*g).kind, Kind::Array(_)));
        if is_arr && depth >= 1.0 {
            let inner = array_like(it, x)?;
            it.enter()?;
            let r = flatten(it, &inner, depth - 1.0, out);
            it.leave();
            r?;
        } else {
            out.push(x.clone());
        }
        if out.len() > MAX_ARRAY {
            return Err(it.err(ErrKind::Range, "Invalid array length"));
        }
    }
    Ok(())
}

fn install_array(it: &mut Interp) {
    let proto = it.p.array.unwrap();
    let c = ctor(
        it,
        "Array",
        |it, a| {
            if a.args.len() == 1 {
                if let Value::Num(n) = a.get(0) {
                    if n < 0.0 || n.fract() != 0.0 || n as usize > MAX_ARRAY {
                        return Err(it.err(ErrKind::Range, "Invalid array length"));
                    }
                    return Ok(it.new_array(vec![Value::Undef; n as usize]));
                }
            }
            Ok(it.new_array(a.args.to_vec()))
        },
        1,
        proto,
    );
    it.method(c, "isArray", |it, a| Ok(Value::Bool(matches!(a.get(0), Value::Obj(g) if matches!(it.heap.get(g).kind, Kind::Array(_))))), 1);
    it.method(c, "of", |it, a| Ok(it.new_array(a.args.to_vec())), 0);
    it.method(c, "from", |it, a| {
        let src = a.get(0);
        if src.is_nullish() {
            return it.type_err("Array.from requires an array-like object");
        }
        let iterable = match &src {
            Value::Str(_) => true,
            Value::Obj(_) => !matches!(it.get(&src, "@@iterator")?, Value::Undef | Value::Null),
            _ => false,
        };
        let items = if iterable { it.collect(&src)? } else { array_like(it, &src)? };
        let f = a.get(1);
        if it.is_callable(&f) {
            let mut out = Vec::with_capacity(items.len());
            for (i, x) in items.into_iter().enumerate() {
                out.push(it.call(&f, a.get(2), vec![x, num(i as f64)])?);
            }
            return Ok(it.new_array(out));
        }
        Ok(it.new_array(items))
    }, 1);

    it.method(proto, "push", |it, a| {
        let g = this_array(it, a)?;
        let v = arr_mut(it, g);
        if v.len() + a.args.len() > MAX_ARRAY {
            return Err(it.err(ErrKind::Range, "Invalid array length"));
        }
        v.extend(a.args.iter().cloned());
        Ok(num(v.len() as f64))
    }, 1);
    it.method(proto, "pop", |it, a| {
        let g = this_array(it, a)?;
        Ok(arr_mut(it, g).pop().unwrap_or(Value::Undef))
    }, 0);
    it.method(proto, "shift", |it, a| {
        let g = this_array(it, a)?;
        let v = arr_mut(it, g);
        Ok(if v.is_empty() { Value::Undef } else { v.remove(0) })
    }, 0);
    it.method(proto, "unshift", |it, a| {
        let g = this_array(it, a)?;
        let v = arr_mut(it, g);
        if v.len() + a.args.len() > MAX_ARRAY {
            return Err(it.err(ErrKind::Range, "Invalid array length"));
        }
        v.splice(0..0, a.args.iter().cloned());
        Ok(num(v.len() as f64))
    }, 1);
    it.method(proto, "slice", |it, a| {
        let (_, e) = this_elems(it, a)?;
        let s = rel_index(it, &a.get(0), e.len(), 0)?;
        let en = rel_index(it, &a.get(1), e.len(), e.len())?;
        Ok(it.new_array(if s < en { e[s..en].to_vec() } else { Vec::new() }))
    }, 2);
    it.method(proto, "splice", |it, a| {
        let g = this_array(it, a)?;
        let len = arr_len(it, g);
        let s = rel_index(it, &a.get(0), len, 0)?;
        let del = if a.args.is_empty() {
            0
        } else if a.args.len() == 1 {
            len - s
        } else {
            let d = it.to_num(&a.get(1))?;
            (if d.is_nan() { 0.0 } else { d.max(0.0) } as usize).min(len - s)
        };
        let v = arr_mut(it, g);
        let removed: Vec<Value> = v.splice(s..s + del, a.args.iter().skip(2).cloned()).collect();
        Ok(it.new_array(removed))
    }, 2);
    it.method(proto, "concat", |it, a| {
        let (_, mut out) = this_elems(it, a)?;
        for x in a.args {
            let is_arr = matches!(x, Value::Obj(g) if matches!(it.heap.get(*g).kind, Kind::Array(_)));
            if is_arr {
                out.extend(array_like(it, x)?);
            } else {
                out.push(x.clone());
            }
            if out.len() > MAX_ARRAY {
                return Err(it.err(ErrKind::Range, "Invalid array length"));
            }
        }
        Ok(it.new_array(out))
    }, 1);
    it.method(proto, "join", |it, a| {
        let (_, e) = this_elems(it, a)?;
        let sep: JsStr = match a.get(0) {
            Value::Undef => ",".into(),
            v => it.to_str(&v)?,
        };
        join(it, &e, &sep)
    }, 1);
    it.method(proto, "toString", |it, a| {
        let (_, e) = this_elems(it, a)?;
        join(it, &e, ",")
    }, 0);
    it.method(proto, "toLocaleString", |it, a| {
        let (_, e) = this_elems(it, a)?;
        join(it, &e, ",")
    }, 0);
    it.method(proto, "reverse", |it, a| {
        let g = this_array(it, a)?;
        arr_mut(it, g).reverse();
        Ok(a.this.clone())
    }, 0);
    it.method(proto, "indexOf", |it, a| {
        let (_, e) = this_elems(it, a)?;
        let from = rel_index(it, &a.get(1), e.len(), 0)?;
        let t = a.get(0);
        Ok(num(e.iter().skip(from).position(|x| Interp::strict_eq(x, &t)).map(|p| (p + from) as f64).unwrap_or(-1.0)))
    }, 1);
    it.method(proto, "lastIndexOf", |it, a| {
        let (_, e) = this_elems(it, a)?;
        let t = a.get(0);
        Ok(num(e.iter().rposition(|x| Interp::strict_eq(x, &t)).map(|p| p as f64).unwrap_or(-1.0)))
    }, 1);
    it.method(proto, "includes", |it, a| {
        let (_, e) = this_elems(it, a)?;
        let t = a.get(0);
        Ok(Value::Bool(e.iter().any(|x| Interp::same_value_zero(x, &t))))
    }, 1);
    fn find(it: &mut Interp, a: &A, want_index: bool, from_end: bool) -> R<Value> {
        let (this, e) = this_elems(it, a)?;
        let f = callback(it, a)?;
        let order: Vec<usize> = if from_end { (0..e.len()).rev().collect() } else { (0..e.len()).collect() };
        for i in order {
            let x = e[i].clone();
            let r = it.call(&f, a.get(1), vec![x.clone(), num(i as f64), this.clone()])?;
            if Interp::truthy(&r) {
                return Ok(if want_index { num(i as f64) } else { x });
            }
        }
        Ok(if want_index { num(-1.0) } else { Value::Undef })
    }
    it.method(proto, "find", |it, a| find(it, a, false, false), 1);
    it.method(proto, "findIndex", |it, a| find(it, a, true, false), 1);
    it.method(proto, "findLast", |it, a| find(it, a, false, true), 1);
    it.method(proto, "findLastIndex", |it, a| find(it, a, true, true), 1);
    it.method(proto, "filter", |it, a| {
        let (this, e) = this_elems(it, a)?;
        let f = callback(it, a)?;
        let mut out = Vec::new();
        for (i, x) in e.into_iter().enumerate() {
            let r = it.call(&f, a.get(1), vec![x.clone(), num(i as f64), this.clone()])?;
            if Interp::truthy(&r) {
                out.push(x);
            }
        }
        Ok(it.new_array(out))
    }, 1);
    it.method(proto, "map", |it, a| {
        let (this, e) = this_elems(it, a)?;
        let f = callback(it, a)?;
        let mut out = Vec::with_capacity(e.len());
        for (i, x) in e.into_iter().enumerate() {
            out.push(it.call(&f, a.get(1), vec![x, num(i as f64), this.clone()])?);
        }
        Ok(it.new_array(out))
    }, 1);
    it.method(proto, "forEach", |it, a| {
        let this = a.this.clone();
        let f = callback(it, a)?;
        // Re-read the length each time: the callback may change the array.
        if let Value::Obj(g) = &this {
            if matches!(it.heap.get(*g).kind, Kind::Array(_)) {
                let mut i = 0;
                while i < arr_len(it, *g) {
                    let x = arr_get(it, *g, i);
                    it.call(&f, a.get(1), vec![x, num(i as f64), this.clone()])?;
                    i += 1;
                }
                return Ok(Value::Undef);
            }
        }
        let (_, e) = this_elems(it, a)?;
        for (i, x) in e.into_iter().enumerate() {
            it.call(&f, a.get(1), vec![x, num(i as f64), this.clone()])?;
        }
        Ok(Value::Undef)
    }, 1);
    fn some_every(it: &mut Interp, a: &A, every: bool) -> R<Value> {
        let (this, e) = this_elems(it, a)?;
        let f = callback(it, a)?;
        for (i, x) in e.into_iter().enumerate() {
            let r = it.call(&f, a.get(1), vec![x, num(i as f64), this.clone()])?;
            if Interp::truthy(&r) != every {
                return Ok(Value::Bool(!every));
            }
        }
        Ok(Value::Bool(every))
    }
    it.method(proto, "some", |it, a| some_every(it, a, false), 1);
    it.method(proto, "every", |it, a| some_every(it, a, true), 1);
    fn reduce(it: &mut Interp, a: &A, right: bool) -> R<Value> {
        let (this, e) = this_elems(it, a)?;
        let f = callback(it, a)?;
        let mut idx: Vec<usize> = (0..e.len()).collect();
        if right {
            idx.reverse();
        }
        let mut iter = idx.into_iter();
        let mut acc = if a.args.len() >= 2 {
            a.get(1)
        } else {
            match iter.next() {
                Some(i) => e[i].clone(),
                None => return it.type_err("Reduce of empty array with no initial value"),
            }
        };
        for i in iter {
            acc = it.call(&f, Value::Undef, vec![acc, e[i].clone(), num(i as f64), this.clone()])?;
        }
        Ok(acc)
    }
    it.method(proto, "reduce", |it, a| reduce(it, a, false), 1);
    it.method(proto, "reduceRight", |it, a| reduce(it, a, true), 1);
    it.method(proto, "sort", |it, a| {
        let g = this_array(it, a)?;
        let cmp = a.get(0);
        if !matches!(cmp, Value::Undef) && !it.is_callable(&cmp) {
            return it.type_err("The comparison function must be either a function or undefined");
        }
        let v = arr_mut(it, g).clone();
        let sorted = merge_sort(it, v, &cmp)?;
        *arr_mut(it, g) = sorted;
        Ok(a.this.clone())
    }, 1);
    it.method(proto, "fill", |it, a| {
        let g = this_array(it, a)?;
        let len = arr_len(it, g);
        let s = rel_index(it, &a.get(1), len, 0)?;
        let e = rel_index(it, &a.get(2), len, len)?;
        let v = arr_mut(it, g);
        for x in v.iter_mut().take(e).skip(s) {
            *x = a.get(0);
        }
        Ok(a.this.clone())
    }, 1);
    it.method(proto, "at", |it, a| {
        let (_, e) = this_elems(it, a)?;
        let n = it.to_num(&a.get(0))?;
        let n = if n.is_nan() { 0.0 } else { n.trunc() };
        let i = if n < 0.0 { e.len() as f64 + n } else { n };
        Ok(if i >= 0.0 && (i as usize) < e.len() { e[i as usize].clone() } else { Value::Undef })
    }, 1);
    it.method(proto, "flat", |it, a| {
        let (_, e) = this_elems(it, a)?;
        let d = match a.get(0) {
            Value::Undef => 1.0,
            v => it.to_num(&v)?,
        };
        let mut out = Vec::new();
        flatten(it, &e, d, &mut out)?;
        Ok(it.new_array(out))
    }, 0);
    it.method(proto, "flatMap", |it, a| {
        let (this, e) = this_elems(it, a)?;
        let f = callback(it, a)?;
        let mut mapped = Vec::with_capacity(e.len());
        for (i, x) in e.into_iter().enumerate() {
            mapped.push(it.call(&f, a.get(1), vec![x, num(i as f64), this.clone()])?);
        }
        let mut out = Vec::new();
        flatten(it, &mapped, 1.0, &mut out)?;
        Ok(it.new_array(out))
    }, 1);
    it.method(proto, "keys", |it, a| make_iter(it, a.this.clone(), IterKind::Keys), 0);
    it.method(proto, "entries", |it, a| make_iter(it, a.this.clone(), IterKind::Entries), 0);
    let values = it.method(proto, "values", array_values, 0);
    it.hide(proto, "@@iterator", Value::Obj(values));

    // The shared iterator prototype: arrays, strings, maps and sets.
    let ip = it.p.iter.unwrap();
    it.method(ip, "next", |it, a| {
        let Value::Obj(g) = a.this else { return it.type_err("next called on a non-iterator") };
        let Kind::Iter { target, pos, kind } = it.heap.get(g).kind.clone() else { return it.type_err("next called on a non-iterator") };
        let item: Option<(Value, Value)> = match &target {
            Value::Str(s) => {
                // Iterates characters; pos counts them.
                str_at(s, pos).map(|c| (num(pos as f64), Value::Str(c.to_string().into())))
            }
            Value::Obj(t) => match &it.heap.get(*t).kind {
                Kind::Array(v) => v.get(pos).map(|x| (num(pos as f64), x.clone())),
                Kind::Map(m) | Kind::Set(m) => {
                    let mut p = pos;
                    let mut found = None;
                    while p < m.entries.len() {
                        if let Some((k, v)) = &m.entries[p] {
                            found = Some((k.clone(), v.clone()));
                            break;
                        }
                        p += 1;
                    }
                    if let Kind::Iter { pos, .. } = &mut it.heap.get_mut(g).kind {
                        *pos = p;
                    }
                    found
                }
                _ => {
                    let items = array_like(it, &target)?;
                    items.get(pos).map(|x| (num(pos as f64), x.clone()))
                }
            },
            _ => None,
        };
        if let Kind::Iter { pos, .. } = &mut it.heap.get_mut(g).kind {
            *pos += 1;
        }
        let r = it.new_object();
        match item {
            Some((k, v)) => {
                let is_set = matches!(&target, Value::Obj(t) if matches!(it.heap.get(*t).kind, Kind::Set(_)));
                let v = match kind {
                    IterKind::Keys => k,
                    IterKind::Values => {
                        if is_set {
                            k
                        } else {
                            v
                        }
                    }
                    IterKind::Entries => {
                        let v = if is_set { k.clone() } else { v };
                        it.new_array(vec![k, v])
                    }
                };
                it.set_prop(r, "value", v);
                it.set_prop(r, "done", Value::Bool(false));
            }
            None => {
                it.set_prop(r, "value", Value::Undef);
                it.set_prop(r, "done", Value::Bool(true));
            }
        }
        Ok(Value::Obj(r))
    }, 0);
    it.method(ip, "@@iterator", |_, a| Ok(a.this.clone()), 0);
}

fn join(it: &mut Interp, e: &[Value], sep: &str) -> R<Value> {
    let mut s = String::new();
    for (i, x) in e.iter().enumerate() {
        if i > 0 {
            s.push_str(sep);
        }
        if !x.is_nullish() {
            let t = it.to_str(x)?;
            s.push_str(&t);
        }
        if s.len() > MAX_STRING {
            return Err(it.err(ErrKind::Range, "Invalid string length"));
        }
    }
    Ok(Value::Str(s.into()))
}

// ---- String ---------------------------------------------------------------------------

fn this_str(it: &mut Interp, a: &A) -> R<JsStr> {
    match &a.this {
        Value::Str(s) => Ok(s.clone()),
        Value::Undef | Value::Null => it.type_err("String.prototype method called on null or undefined"),
        v => it.to_str(v),
    }
}

fn chars(s: &str) -> Vec<char> {
    s.chars().collect()
}

fn substr(c: &[char], a: usize, b: usize) -> Value {
    if a >= b {
        return Value::str("");
    }
    Value::Str(c[a.min(c.len())..b.min(c.len())].iter().collect::<String>().into())
}

fn find_sub(hay: &[char], needle: &[char], from: usize) -> Option<usize> {
    if needle.is_empty() {
        return Some(from.min(hay.len()));
    }
    if needle.len() > hay.len() {
        return None;
    }
    (from..=hay.len() - needle.len()).find(|&i| hay[i..i + needle.len()] == *needle)
}

fn regexp_of(it: &Interp, v: &Value) -> Option<Rc<super::regex::Regex>> {
    match v {
        Value::Obj(g) => match &it.heap.get(*g).kind {
            Kind::RegExp(r) => Some(r.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// All matches of `re` in `s` (just the first unless global), as captures.
fn regex_matches(it: &mut Interp, re: &super::regex::Regex, s: &[char], all: bool) -> R<Vec<super::regex::Caps>> {
    let mut out = Vec::new();
    let mut at = 0;
    while at <= s.len() {
        match re.exec_at(s, at) {
            Ok(Some(c)) => {
                let (a, b) = c[0].unwrap();
                at = if b == a { b + 1 } else { b };
                out.push(c);
                if !all {
                    break;
                }
                it.tick()?;
            }
            Ok(None) => break,
            Err(_) => return Err(it.err(ErrKind::Range, "regular expression too complex for this input")),
        }
    }
    Ok(out)
}

fn expand_replacement(rep: &str, s: &[char], caps: &super::regex::Caps, names: &[(String, usize)]) -> String {
    let (ma, mb) = caps[0].unwrap();
    let r: Vec<char> = rep.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < r.len() {
        if r[i] == '$' && i + 1 < r.len() {
            match r[i + 1] {
                '$' => {
                    out.push('$');
                    i += 2;
                    continue;
                }
                '&' => {
                    out.extend(&s[ma..mb]);
                    i += 2;
                    continue;
                }
                '`' => {
                    out.extend(&s[..ma]);
                    i += 2;
                    continue;
                }
                '\'' => {
                    out.extend(&s[mb..]);
                    i += 2;
                    continue;
                }
                '<' => {
                    if let Some(end) = r[i + 2..].iter().position(|&c| c == '>') {
                        let name: String = r[i + 2..i + 2 + end].iter().collect();
                        if let Some((_, gi)) = names.iter().find(|(n, _)| *n == name) {
                            if let Some(Some((x, y))) = caps.get(*gi) {
                                out.extend(&s[*x..*y]);
                            }
                            i += 3 + end;
                            continue;
                        }
                    }
                }
                d if d.is_ascii_digit() => {
                    let one = d.to_digit(10).unwrap() as usize;
                    let two = r.get(i + 2).and_then(|c| c.to_digit(10)).map(|e| one * 10 + e as usize);
                    let (g, used) = match two {
                        Some(t) if t > 0 && t < caps.len() => (t, 3),
                        _ => (one, 2),
                    };
                    if g > 0 && g < caps.len() {
                        if let Some((x, y)) = caps[g] {
                            out.extend(&s[x..y]);
                        }
                        i += used;
                        continue;
                    }
                }
                _ => {}
            }
        }
        out.push(r[i]);
        i += 1;
    }
    out
}

fn replace(it: &mut Interp, a: &A, all: bool) -> R<Value> {
    let s = this_str(it, a)?;
    let sc = chars(&s);
    let pat = a.get(0);
    let rep = a.get(1);
    let func = it.is_callable(&rep);
    let rep_s = if func { "".into() } else { it.to_str(&rep)? };
    let (matches, names): (Vec<super::regex::Caps>, Vec<(String, usize)>) = match regexp_of(it, &pat) {
        Some(re) => {
            if all && !re.global {
                return it.type_err("replaceAll must be called with a global RegExp");
            }
            (regex_matches(it, &re, &sc, re.global)?, re.names.clone())
        }
        None => {
            let p = it.to_str(&pat)?;
            let pc = chars(&p);
            let mut m = Vec::new();
            let mut from = 0;
            while let Some(i) = find_sub(&sc, &pc, from) {
                m.push(vec![Some((i, i + pc.len()))]);
                if !all {
                    break;
                }
                from = if pc.is_empty() { i + 1 } else { i + pc.len() };
                if from > sc.len() {
                    break;
                }
            }
            (m, Vec::new())
        }
    };
    let mut out = String::new();
    let mut last = 0;
    for caps in &matches {
        let (ma, mb) = caps[0].unwrap();
        out.extend(&sc[last..ma]);
        if func {
            let mut args: Vec<Value> = caps.iter().map(|c| c.map(|(x, y)| Value::Str(sc[x..y].iter().collect::<String>().into())).unwrap_or(Value::Undef)).collect();
            args.push(num(ma as f64));
            args.push(Value::Str(s.clone()));
            let r = it.call(&rep, Value::Undef, args)?;
            let t = it.to_str(&r)?;
            out.push_str(&t);
        } else {
            out.push_str(&expand_replacement(&rep_s, &sc, caps, &names));
        }
        last = mb;
        if out.len() > MAX_STRING {
            return Err(it.err(ErrKind::Range, "Invalid string length"));
        }
    }
    out.extend(&sc[last.min(sc.len())..]);
    Ok(Value::Str(out.into()))
}

/// RegExp exec: the match array (with index, input and groups), or null.
fn regexp_exec(it: &mut Interp, rx: &Value, s: &JsStr) -> R<Value> {
    let Some(re) = regexp_of(it, rx) else { return it.type_err("RegExp method called on a non-RegExp") };
    let sc = chars(s);
    let sticky_or_global = re.global || re.sticky;
    let start = if sticky_or_global {
        let li = it.get(rx, "lastIndex")?;
        let n = it.to_num(&li)?;
        if n.is_nan() || n < 0.0 {
            0
        } else {
            n as usize
        }
    } else {
        0
    };
    if start > sc.len() {
        it.put(rx, "lastIndex", num(0.0))?;
        return Ok(Value::Null);
    }
    let r = match re.exec_at(&sc, start) {
        Ok(r) => r,
        Err(_) => return Err(it.err(ErrKind::Range, "regular expression too complex for this input")),
    };
    let Some(caps) = r else {
        if sticky_or_global {
            it.put(rx, "lastIndex", num(0.0))?;
        }
        return Ok(Value::Null);
    };
    let (ma, mb) = caps[0].unwrap();
    if sticky_or_global {
        it.put(rx, "lastIndex", num(mb as f64))?;
    }
    let items: Vec<Value> = caps.iter().map(|c| c.map(|(x, y)| Value::Str(sc[x..y].iter().collect::<String>().into())).unwrap_or(Value::Undef)).collect();
    let arr = it.new_array(items.clone());
    let g = arr.as_obj().unwrap();
    it.set_prop(g, "index", num(ma as f64));
    it.set_prop(g, "input", Value::Str(s.clone()));
    let groups = if re.names.is_empty() {
        Value::Undef
    } else {
        let o = it.new_object();
        for (n, i) in &re.names {
            it.set_prop(o, n, items.get(*i).cloned().unwrap_or(Value::Undef));
        }
        Value::Obj(o)
    };
    it.set_prop(g, "groups", groups);
    Ok(arr)
}

fn install_string(it: &mut Interp) {
    let proto = it.p.string.unwrap();
    let c = ctor(
        it,
        "String",
        |it, a| {
            let s = match a.args.first() {
                None => Value::str(""),
                Some(v) => Value::Str(it.to_str(v)?),
            };
            if a.new_target.is_some() {
                let p = it.p.string;
                return Ok(Value::Obj(it.alloc(Obj::new(p, Kind::Boxed(s)))));
            }
            Ok(s)
        },
        1,
        proto,
    );
    it.method(c, "fromCharCode", |it, a| {
        let mut units = Vec::new();
        for x in a.args {
            units.push((it.to_num(x)? as i64 as u32 & 0xFFFF) as u16);
        }
        Ok(Value::Str(char::decode_utf16(units).map(|r| r.unwrap_or('\u{FFFD}')).collect::<String>().into()))
    }, 1);
    it.method(c, "fromCodePoint", |it, a| {
        let mut s = String::new();
        for x in a.args {
            let n = it.to_num(x)?;
            match char::from_u32(n as u32).filter(|_| n >= 0.0 && n.fract() == 0.0) {
                Some(ch) => s.push(ch),
                None => return Err(it.err(ErrKind::Range, "Invalid code point")),
            }
        }
        Ok(Value::Str(s.into()))
    }, 1);
    it.method(c, "raw", |it, a| {
        let strs = a.get(0);
        let raw = it.get(&strs, "raw")?;
        let parts = array_like(it, &raw)?;
        let mut s = String::new();
        for (i, p) in parts.iter().enumerate() {
            s.push_str(&it.to_str(p)?);
            if i + 1 < parts.len() {
                if let Some(v) = a.args.get(i + 1) {
                    s.push_str(&it.to_str(v)?);
                }
            }
        }
        Ok(Value::Str(s.into()))
    }, 1);

    it.method(proto, "toString", |it, a| match &a.this {
        Value::Str(s) => Ok(Value::Str(s.clone())),
        Value::Obj(g) => match &it.heap.get(*g).kind {
            Kind::Boxed(v @ Value::Str(_)) => Ok(v.clone()),
            _ => it.type_err("String.prototype.toString requires a string"),
        },
        _ => it.type_err("String.prototype.toString requires a string"),
    }, 0);
    it.method(proto, "valueOf", |it, a| match &a.this {
        Value::Str(s) => Ok(Value::Str(s.clone())),
        Value::Obj(g) => match &it.heap.get(*g).kind {
            Kind::Boxed(v) => Ok(v.clone()),
            _ => it.type_err("String.prototype.valueOf requires a string"),
        },
        _ => it.type_err("String.prototype.valueOf requires a string"),
    }, 0);
    it.method(proto, "charAt", |it, a| {
        let s = this_str(it, a)?;
        let i = it.to_num(&a.get(0))?;
        let i = if i.is_nan() { 0.0 } else { i };
        Ok(if i < 0.0 { Value::str("") } else { str_at(&s, i as usize).map(|c| Value::Str(c.to_string().into())).unwrap_or(Value::str("")) })
    }, 1);
    it.method(proto, "charCodeAt", |it, a| {
        let s = this_str(it, a)?;
        let i = it.to_num(&a.get(0))?;
        let i = if i.is_nan() { 0.0 } else { i };
        Ok(num(if i < 0.0 {
            f64::NAN
        } else {
            str_at(&s, i as usize).map(|c| {
                let mut b = [0u16; 2];
                c.encode_utf16(&mut b)[0] as f64
            }).unwrap_or(f64::NAN)
        }))
    }, 1);
    it.method(proto, "codePointAt", |it, a| {
        let s = this_str(it, a)?;
        let i = it.to_num(&a.get(0))?;
        let i = if i.is_nan() { 0.0 } else { i };
        Ok(if i < 0.0 { Value::Undef } else { str_at(&s, i as usize).map(|c| num(c as u32 as f64)).unwrap_or(Value::Undef) })
    }, 1);
    it.method(proto, "at", |it, a| {
        let s = this_str(it, a)?;
        let len = str_len(&s) as f64;
        let n = it.to_num(&a.get(0))?;
        let n = if n.is_nan() { 0.0 } else { n.trunc() };
        let i = if n < 0.0 { len + n } else { n };
        Ok(if i >= 0.0 && i < len { str_at(&s, i as usize).map(|c| Value::Str(c.to_string().into())).unwrap_or(Value::Undef) } else { Value::Undef })
    }, 1);
    it.method(proto, "indexOf", |it, a| {
        let s = chars(&this_str(it, a)?);
        let n = chars(&it.to_str(&a.get(0))?);
        let from = rel_index(it, &a.get(1), s.len(), 0)?;
        let from = if matches!(a.get(1), Value::Num(x) if x < 0.0) { 0 } else { from };
        Ok(num(find_sub(&s, &n, from).map(|i| i as f64).unwrap_or(-1.0)))
    }, 1);
    it.method(proto, "lastIndexOf", |it, a| {
        let s = chars(&this_str(it, a)?);
        let n = chars(&it.to_str(&a.get(0))?);
        if n.len() > s.len() {
            return Ok(num(-1.0));
        }
        let max = match a.get(1) {
            Value::Undef => s.len() - n.len(),
            v => {
                let x = it.to_num(&v)?;
                if x.is_nan() {
                    s.len() - n.len()
                } else {
                    (x.max(0.0) as usize).min(s.len() - n.len())
                }
            }
        };
        Ok(num((0..=max).rev().find(|&i| s[i..i + n.len()] == *n).map(|i| i as f64).unwrap_or(-1.0)))
    }, 1);
    it.method(proto, "includes", |it, a| {
        if regexp_of(it, &a.get(0)).is_some() {
            return it.type_err("First argument to String.prototype.includes must not be a regular expression");
        }
        let s = this_str(it, a)?;
        let n = it.to_str(&a.get(0))?;
        let sc = chars(&s);
        let from = rel_index(it, &a.get(1), sc.len(), 0)?;
        Ok(Value::Bool(find_sub(&sc, &chars(&n), from).is_some()))
    }, 1);
    it.method(proto, "startsWith", |it, a| {
        let s = chars(&this_str(it, a)?);
        let n = chars(&it.to_str(&a.get(0))?);
        let from = rel_index(it, &a.get(1), s.len(), 0)?;
        Ok(Value::Bool(s.len() >= from + n.len() && s[from..from + n.len()] == *n))
    }, 1);
    it.method(proto, "endsWith", |it, a| {
        let s = chars(&this_str(it, a)?);
        let n = chars(&it.to_str(&a.get(0))?);
        let end = rel_index(it, &a.get(1), s.len(), s.len())?;
        Ok(Value::Bool(end >= n.len() && s[end - n.len()..end] == *n))
    }, 1);
    it.method(proto, "slice", |it, a| {
        let s = chars(&this_str(it, a)?);
        let x = rel_index(it, &a.get(0), s.len(), 0)?;
        let y = rel_index(it, &a.get(1), s.len(), s.len())?;
        Ok(substr(&s, x, y))
    }, 2);
    it.method(proto, "substring", |it, a| {
        let s = chars(&this_str(it, a)?);
        let clamp = |it: &mut Interp, v: &Value, d: usize| -> R<usize> {
            if matches!(v, Value::Undef) {
                return Ok(d);
            }
            let n = it.to_num(v)?;
            Ok(if n.is_nan() || n < 0.0 { 0 } else { (n as usize).min(s.len()) })
        };
        let x = clamp(it, &a.get(0), 0)?;
        let y = clamp(it, &a.get(1), s.len())?;
        Ok(substr(&s, x.min(y), x.max(y)))
    }, 2);
    it.method(proto, "substr", |it, a| {
        let s = chars(&this_str(it, a)?);
        let x = rel_index(it, &a.get(0), s.len(), 0)?;
        let n = match a.get(1) {
            Value::Undef => s.len(),
            v => {
                let n = it.to_num(&v)?;
                if n.is_nan() || n < 0.0 {
                    0
                } else {
                    n as usize
                }
            }
        };
        Ok(substr(&s, x, x.saturating_add(n)))
    }, 2);
    it.method(proto, "toUpperCase", |it, a| Ok(Value::Str(this_str(it, a)?.to_uppercase().into())), 0);
    it.method(proto, "toLowerCase", |it, a| Ok(Value::Str(this_str(it, a)?.to_lowercase().into())), 0);
    it.method(proto, "toLocaleUpperCase", |it, a| Ok(Value::Str(this_str(it, a)?.to_uppercase().into())), 0);
    it.method(proto, "toLocaleLowerCase", |it, a| Ok(Value::Str(this_str(it, a)?.to_lowercase().into())), 0);
    let ws = |c: char| c.is_whitespace() || c == '\u{FEFF}';
    let _ = ws;
    it.method(proto, "trim", |it, a| Ok(Value::Str(this_str(it, a)?.trim_matches(|c: char| c.is_whitespace() || c == '\u{FEFF}').into())), 0);
    it.method(proto, "trimStart", |it, a| Ok(Value::Str(this_str(it, a)?.trim_start_matches(|c: char| c.is_whitespace() || c == '\u{FEFF}').into())), 0);
    it.method(proto, "trimEnd", |it, a| Ok(Value::Str(this_str(it, a)?.trim_end_matches(|c: char| c.is_whitespace() || c == '\u{FEFF}').into())), 0);
    fn pad(it: &mut Interp, a: &A, start: bool) -> R<Value> {
        let s = this_str(it, a)?;
        let target = it.to_num(&a.get(0))?;
        let len = str_len(&s);
        if target.is_nan() || target as usize <= len {
            return Ok(Value::Str(s));
        }
        let target = target as usize;
        if target > MAX_STRING {
            return Err(it.err(ErrKind::Range, "Invalid string length"));
        }
        let fill: Vec<char> = match a.get(1) {
            Value::Undef => vec![' '],
            v => chars(&it.to_str(&v)?),
        };
        if fill.is_empty() {
            return Ok(Value::Str(s));
        }
        let padding: String = fill.iter().cycle().take(target - len).collect();
        Ok(Value::Str(if start { format!("{padding}{s}") } else { format!("{s}{padding}") }.into()))
    }
    it.method(proto, "padStart", |it, a| pad(it, a, true), 2);
    it.method(proto, "padEnd", |it, a| pad(it, a, false), 2);
    it.method(proto, "repeat", |it, a| {
        let s = this_str(it, a)?;
        let n = it.to_num(&a.get(0))?;
        if n < 0.0 || n.is_infinite() {
            return Err(it.err(ErrKind::Range, "Invalid count value"));
        }
        let n = if n.is_nan() { 0 } else { n as usize };
        if s.len().saturating_mul(n) > MAX_STRING {
            return Err(it.err(ErrKind::Range, "Invalid string length"));
        }
        Ok(Value::Str(s.repeat(n).into()))
    }, 1);
    it.method(proto, "concat", |it, a| {
        let mut s = this_str(it, a)?.to_string();
        for x in a.args {
            s.push_str(&it.to_str(x)?);
            if s.len() > MAX_STRING {
                return Err(it.err(ErrKind::Range, "Invalid string length"));
            }
        }
        Ok(Value::Str(s.into()))
    }, 1);
    it.method(proto, "localeCompare", |it, a| {
        let s = this_str(it, a)?;
        let t = it.to_str(&a.get(0))?;
        let (x, y) = (s.to_lowercase(), t.to_lowercase());
        let o = x.cmp(&y).then(s.cmp(&t));
        Ok(num(match o {
            std::cmp::Ordering::Less => -1.0,
            std::cmp::Ordering::Equal => 0.0,
            std::cmp::Ordering::Greater => 1.0,
        }))
    }, 1);
    it.method(proto, "normalize", |it, a| Ok(Value::Str(this_str(it, a)?)), 0);
    it.method(proto, "split", |it, a| {
        let s = this_str(it, a)?;
        let limit = match a.get(1) {
            Value::Undef => usize::MAX,
            v => it.to_num(&v)? as u32 as usize,
        };
        let sep = a.get(0);
        let sc = chars(&s);
        let mut out: Vec<Value> = Vec::new();
        if limit == 0 {
            return Ok(it.new_array(out));
        }
        if let Some(re) = regexp_of(it, &sep) {
            if sc.is_empty() {
                let m = re.exec_at(&sc, 0).ok().flatten();
                return Ok(it.new_array(if m.is_some() { vec![] } else { vec![Value::Str(s)] }));
            }
            let mut p = 0;
            let mut q = 0;
            while q < sc.len() {
                let m = match re.exec_at(&sc, q) {
                    Ok(m) => m,
                    Err(_) => return Err(it.err(ErrKind::Range, "regular expression too complex for this input")),
                };
                let Some(caps) = m else { break };
                let (ma, mb) = caps[0].unwrap();
                if mb == ma && ma == q || ma >= sc.len() {
                    q = ma.max(q) + 1;
                    if ma >= sc.len() {
                        break;
                    }
                    continue;
                }
                if mb == p {
                    q += 1;
                    continue;
                }
                out.push(substr(&sc, p, ma));
                if out.len() >= limit {
                    return Ok(it.new_array(out));
                }
                for c in caps.iter().skip(1) {
                    out.push(c.map(|(x, y)| substr(&sc, x, y)).unwrap_or(Value::Undef));
                    if out.len() >= limit {
                        return Ok(it.new_array(out));
                    }
                }
                p = mb;
                q = if mb == ma { mb + 1 } else { mb };
                it.tick()?;
            }
            out.push(substr(&sc, p, sc.len()));
            return Ok(it.new_array(out));
        }
        if matches!(sep, Value::Undef) {
            return Ok(it.new_array(vec![Value::Str(s)]));
        }
        let sepc = chars(&it.to_str(&sep)?);
        if sepc.is_empty() {
            out = sc.iter().take(limit).map(|c| Value::Str(c.to_string().into())).collect();
            return Ok(it.new_array(out));
        }
        let mut from = 0;
        while let Some(i) = find_sub(&sc, &sepc, from) {
            out.push(substr(&sc, from, i));
            if out.len() >= limit {
                return Ok(it.new_array(out));
            }
            from = i + sepc.len();
        }
        out.push(substr(&sc, from, sc.len()));
        Ok(it.new_array(out))
    }, 2);
    it.method(proto, "replace", |it, a| replace(it, a, false), 2);
    it.method(proto, "replaceAll", |it, a| replace(it, a, true), 2);
    it.method(proto, "match", |it, a| {
        let s = this_str(it, a)?;
        let pat = a.get(0);
        let rx = match regexp_of(it, &pat) {
            Some(_) => pat,
            None => {
                let src: JsStr = if matches!(pat, Value::Undef) { "(?:)".into() } else { it.to_str(&pat)? };
                let re = it.regex(&src, &"".into())?;
                it.new_regexp(re)
            }
        };
        let re = regexp_of(it, &rx).unwrap();
        if !re.global {
            return regexp_exec(it, &rx, &s);
        }
        let sc = chars(&s);
        let m = regex_matches(it, &re, &sc, true)?;
        it.put(&rx, "lastIndex", num(0.0))?;
        if m.is_empty() {
            return Ok(Value::Null);
        }
        let items = m.iter().map(|c| {
            let (x, y) = c[0].unwrap();
            substr(&sc, x, y)
        }).collect();
        Ok(it.new_array(items))
    }, 1);
    it.method(proto, "matchAll", |it, a| {
        let s = this_str(it, a)?;
        let pat = a.get(0);
        let re = match regexp_of(it, &pat) {
            Some(r) => {
                if !r.global {
                    return it.type_err("matchAll must be called with a global RegExp");
                }
                r
            }
            None => {
                let src = it.to_str(&pat)?;
                it.regex(&src, &"g".into())?
            }
        };
        let sc = chars(&s);
        let m = regex_matches(it, &re, &sc, true)?;
        let mut out = Vec::new();
        for caps in m {
            let items: Vec<Value> = caps.iter().map(|c| c.map(|(x, y)| substr(&sc, x, y)).unwrap_or(Value::Undef)).collect();
            let arr = it.new_array(items);
            let g = arr.as_obj().unwrap();
            it.set_prop(g, "index", num(caps[0].unwrap().0 as f64));
            it.set_prop(g, "input", Value::Str(s.clone()));
            out.push(arr);
        }
        let arr = it.new_array(out);
        make_iter(it, arr, IterKind::Values)
    }, 1);
    it.method(proto, "search", |it, a| {
        let s = this_str(it, a)?;
        let pat = a.get(0);
        let re = match regexp_of(it, &pat) {
            Some(r) => r,
            None => {
                let src = it.to_str(&pat)?;
                it.regex(&src, &"".into())?
            }
        };
        let sc = chars(&s);
        let m = regex_matches(it, &re, &sc, false)?;
        Ok(num(m.first().map(|c| c[0].unwrap().0 as f64).unwrap_or(-1.0)))
    }, 1);
    it.method(proto, "@@iterator", |it, a| {
        let s = this_str(it, a)?;
        make_iter(it, Value::Str(s), IterKind::Values)
    }, 0);
    // Legacy HTML helpers some old pages still call.
    it.method(proto, "anchor", |it, a| {
        let s = this_str(it, a)?;
        let n = it.to_str(&a.get(0))?;
        Ok(Value::Str(format!("<a name=\"{n}\">{s}</a>").into()))
    }, 1);
}

// ---- Number and Boolean ---------------------------------------------------------------

fn this_num(it: &mut Interp, a: &A) -> R<f64> {
    match &a.this {
        Value::Num(n) => Ok(*n),
        Value::Obj(g) => match &it.heap.get(*g).kind {
            Kind::Boxed(Value::Num(n)) => Ok(*n),
            _ => it.type_err("Number.prototype method called on a non-number"),
        },
        _ => it.type_err("Number.prototype method called on a non-number"),
    }
}

fn to_radix(v: f64, radix: u32) -> String {
    if v.is_nan() {
        return "NaN".into();
    }
    if v.is_infinite() {
        return if v > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    let neg = v < 0.0;
    let v = v.abs();
    let mut int = v.trunc();
    let mut frac = v - int;
    let mut digits = Vec::new();
    if int == 0.0 {
        digits.push('0');
    }
    while int >= 1.0 && digits.len() < 1100 {
        let d = (int % radix as f64) as u32;
        digits.push(std::char::from_digit(d, radix).unwrap_or('0'));
        int = (int / radix as f64).trunc();
    }
    digits.reverse();
    let mut s: String = digits.into_iter().collect();
    if frac > 0.0 {
        s.push('.');
        let mut n = 0;
        while frac > 0.0 && n < 20 {
            frac *= radix as f64;
            let d = frac.trunc() as u32;
            s.push(std::char::from_digit(d, radix).unwrap_or('0'));
            frac -= d as f64;
            n += 1;
        }
    }
    if neg {
        format!("-{s}")
    } else {
        s
    }
}

pub fn to_fixed(x: f64, d: usize) -> String {
    if !x.is_finite() || x.abs() >= 1e21 {
        return num_to_str(x);
    }
    // Ties go away from zero, as the spec's "larger n" rule says.
    let scaled = x.abs() * 10f64.powi(d as i32);
    let mut s = format!("{:.*}", d, x.abs());
    if (scaled - scaled.trunc() - 0.5).abs() < 1e-9 && scaled < 1e15 {
        let n = scaled.trunc() + 1.0;
        let digits = format!("{n:.0}");
        let digits = format!("{:0>width$}", digits, width = d + 1);
        s = if d == 0 { digits } else { format!("{}.{}", &digits[..digits.len() - d], &digits[digits.len() - d..]) };
    }
    if x < 0.0 && s.chars().any(|c| c != '0' && c != '.') {
        format!("-{s}")
    } else {
        s
    }
}

fn group_thousands(s: &str) -> String {
    let (int, frac) = s.split_once('.').map(|(a, b)| (a, Some(b))).unwrap_or((s, None));
    let (neg, int) = int.strip_prefix('-').map(|i| (true, i)).unwrap_or((false, int));
    let mut out = String::new();
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    let mut r = if neg { format!("-{out}") } else { out };
    if let Some(f) = frac {
        r.push('.');
        r.push_str(f);
    }
    r
}

fn install_number(it: &mut Interp) {
    let proto = it.p.number.unwrap();
    let c = ctor(
        it,
        "Number",
        |it, a| {
            let n = match a.args.first() {
                None => 0.0,
                Some(v) => it.to_num(v)?,
            };
            if a.new_target.is_some() {
                let p = it.p.number;
                return Ok(Value::Obj(it.alloc(Obj::new(p, Kind::Boxed(num(n))))));
            }
            Ok(num(n))
        },
        1,
        proto,
    );
    for (k, v) in [
        ("MAX_SAFE_INTEGER", 9007199254740991.0),
        ("MIN_SAFE_INTEGER", -9007199254740991.0),
        ("EPSILON", f64::EPSILON),
        ("MAX_VALUE", f64::MAX),
        ("MIN_VALUE", 5e-324),
        ("POSITIVE_INFINITY", f64::INFINITY),
        ("NEGATIVE_INFINITY", f64::NEG_INFINITY),
        ("NaN", f64::NAN),
    ] {
        it.heap.get_mut(c).props.set(k.into(), Slot::Data(num(v), 0));
    }
    it.method(c, "isInteger", |_, a| Ok(Value::Bool(matches!(a.get(0), Value::Num(n) if n.is_finite() && n.fract() == 0.0))), 1);
    it.method(c, "isSafeInteger", |_, a| Ok(Value::Bool(matches!(a.get(0), Value::Num(n) if n.is_finite() && n.fract() == 0.0 && n.abs() <= 9007199254740991.0))), 1);
    it.method(c, "isFinite", |_, a| Ok(Value::Bool(matches!(a.get(0), Value::Num(n) if n.is_finite()))), 1);
    it.method(c, "isNaN", |_, a| Ok(Value::Bool(matches!(a.get(0), Value::Num(n) if n.is_nan()))), 1);
    let pf = it.method(c, "parseFloat", parse_float, 1);
    let pi = it.method(c, "parseInt", parse_int, 2);
    let g = it.global;
    it.hide(g, "parseFloat", Value::Obj(pf));
    it.hide(g, "parseInt", Value::Obj(pi));

    it.method(proto, "toString", |it, a| {
        let n = this_num(it, a)?;
        let r = match a.get(0) {
            Value::Undef => 10,
            v => it.to_num(&v)? as u32,
        };
        if !(2..=36).contains(&r) {
            return Err(it.err(ErrKind::Range, "toString() radix must be between 2 and 36"));
        }
        Ok(Value::Str(if r == 10 { num_to_str(n) } else { to_radix(n, r) }.into()))
    }, 1);
    it.method(proto, "toLocaleString", |it, a| {
        let n = this_num(it, a)?;
        let s = if n.fract() == 0.0 || !n.is_finite() { num_to_str(n) } else { to_fixed(n, 3).trim_end_matches('0').trim_end_matches('.').to_string() };
        Ok(Value::Str(if n.is_finite() && n.abs() < 1e21 { group_thousands(&s) } else { s }.into()))
    }, 0);
    it.method(proto, "valueOf", |it, a| Ok(num(this_num(it, a)?)), 0);
    it.method(proto, "toFixed", |it, a| {
        let n = this_num(it, a)?;
        let d = it.to_num(&a.get(0))?;
        let d = if d.is_nan() { 0.0 } else { d };
        if !(0.0..=100.0).contains(&d) {
            return Err(it.err(ErrKind::Range, "toFixed() digits argument must be between 0 and 100"));
        }
        Ok(Value::Str(to_fixed(n, d as usize).into()))
    }, 1);
    it.method(proto, "toPrecision", |it, a| {
        let n = this_num(it, a)?;
        if matches!(a.get(0), Value::Undef) || !n.is_finite() {
            return Ok(Value::Str(num_to_str(n).into()));
        }
        let p = it.to_num(&a.get(0))?;
        if !(1.0..=100.0).contains(&p) {
            return Err(it.err(ErrKind::Range, "toPrecision() argument must be between 1 and 100"));
        }
        let p = p as i32;
        if n == 0.0 {
            return Ok(Value::Str(to_fixed(0.0, (p - 1) as usize).into()));
        }
        let e = n.abs().log10().floor() as i32;
        let s = if e < -6 || e >= p {
            let m = format!("{:.*e}", (p - 1) as usize, n);
            let (mant, ex) = m.split_once('e').unwrap_or((&m, "0"));
            let ex: i32 = ex.parse().unwrap_or(0);
            format!("{mant}e{}{}", if ex >= 0 { "+" } else { "-" }, ex.abs())
        } else {
            to_fixed(n, (p - 1 - e).max(0) as usize)
        };
        Ok(Value::Str(s.into()))
    }, 1);
    it.method(proto, "toExponential", |it, a| {
        let n = this_num(it, a)?;
        let d = match a.get(0) {
            Value::Undef => 6,
            v => it.to_num(&v)? as usize,
        };
        let m = format!("{:.*e}", d.min(100), n);
        let (mant, ex) = m.split_once('e').unwrap_or((&m, "0"));
        let ex: i32 = ex.parse().unwrap_or(0);
        Ok(Value::Str(format!("{mant}e{}{}", if ex >= 0 { "+" } else { "-" }, ex.abs()).into()))
    }, 1);

    let bp = it.p.boolean.unwrap();
    ctor(
        it,
        "Boolean",
        |it, a| {
            let b = Value::Bool(Interp::truthy(&a.get(0)));
            if a.new_target.is_some() {
                let p = it.p.boolean;
                return Ok(Value::Obj(it.alloc(Obj::new(p, Kind::Boxed(b)))));
            }
            Ok(b)
        },
        1,
        bp,
    );
    fn this_bool(it: &mut Interp, a: &A) -> R<bool> {
        match &a.this {
            Value::Bool(b) => Ok(*b),
            Value::Obj(g) => match &it.heap.get(*g).kind {
                Kind::Boxed(Value::Bool(b)) => Ok(*b),
                _ => it.type_err("not a boolean"),
            },
            _ => it.type_err("not a boolean"),
        }
    }
    it.method(bp, "toString", |it, a| Ok(Value::str(if this_bool(it, a)? { "true" } else { "false" })), 0);
    it.method(bp, "valueOf", |it, a| Ok(Value::Bool(this_bool(it, a)?)), 0);
}

fn parse_float(it: &mut Interp, a: &A) -> R<Value> {
    let s = it.to_str(&a.get(0))?;
    let t = s.trim_start();
    for (p, v) in [("Infinity", f64::INFINITY), ("+Infinity", f64::INFINITY), ("-Infinity", f64::NEG_INFINITY)] {
        if t.starts_with(p) {
            return Ok(num(v));
        }
    }
    // The longest prefix that is a decimal literal.
    let b = t.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    let ds = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
    }
    if i == ds || (i == ds + 1 && b[ds] == b'.') {
        return Ok(num(f64::NAN));
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let mut j = i + 1;
        if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
            j += 1;
        }
        let es = j;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        if j > es {
            i = j;
        }
    }
    let mut lit = t[..i].to_string();
    if lit.ends_with('.') {
        lit.push('0');
    }
    Ok(num(lit.replace("+.", "0.").replace("-.", "-0.").parse::<f64>().unwrap_or_else(|_| format!("0{}", &lit).parse().unwrap_or(f64::NAN))))
}

fn parse_int(it: &mut Interp, a: &A) -> R<Value> {
    let s = it.to_str(&a.get(0))?;
    let mut t = s.trim_start();
    let mut neg = false;
    if let Some(r) = t.strip_prefix('-') {
        neg = true;
        t = r;
    } else if let Some(r) = t.strip_prefix('+') {
        t = r;
    }
    let mut radix = match a.get(1) {
        Value::Undef => 0,
        v => f64_to_int32(it.to_num(&v)?),
    };
    if radix != 0 && !(2..=36).contains(&radix) {
        return Ok(num(f64::NAN));
    }
    if (radix == 0 || radix == 16) && (t.starts_with("0x") || t.starts_with("0X")) {
        t = &t[2..];
        radix = 16;
    }
    if radix == 0 {
        radix = 10;
    }
    let mut v = 0f64;
    let mut any = false;
    for c in t.chars() {
        match c.to_digit(radix as u32) {
            Some(d) => {
                v = v * radix as f64 + d as f64;
                any = true;
            }
            None => break,
        }
    }
    if !any {
        return Ok(num(f64::NAN));
    }
    Ok(num(if neg { -v } else { v }))
}

// ---- errors ------------------------------------------------------------------------

fn install_errors(it: &mut Interp, error: Gc) {
    fn make(it: &mut Interp, a: &A, proto: Option<Gc>) -> R<Value> {
        let proto = match &a.new_target {
            Some(nt) => match it.get(nt, "prototype")? {
                Value::Obj(p) => Some(p),
                _ => proto,
            },
            None => proto,
        };
        let mut o = Obj::new(proto, Kind::Error);
        if !matches!(a.get(0), Value::Undef) {
            let m = it.to_str(&a.get(0))?;
            o.props.set("message".into(), Slot::Data(Value::Str(m), F_HIDDEN));
        }
        o.props.set("stack".into(), Slot::Data(Value::str(""), F_HIDDEN));
        let g = it.alloc(o);
        if let Value::Obj(opts) = a.get(1) {
            if it.has_property(opts, "cause")? {
                let c = it.get(&Value::Obj(opts), "cause")?;
                it.hide(g, "cause", c);
            }
        }
        Ok(Value::Obj(g))
    }
    let e = ctor(it, "Error", |it, a| {
        let p = it.p.error;
        make(it, a, p)
    }, 1, error);
    let _ = e;
    it.hide(error, "name", Value::str("Error"));
    it.hide(error, "message", Value::str(""));
    it.method(error, "toString", |it, a| {
        let n = it.get(&a.this, "name")?;
        let m = it.get(&a.this, "message")?;
        let n = if matches!(n, Value::Undef) { "Error".into() } else { it.to_str(&n)? };
        let m = if matches!(m, Value::Undef) { "".into() } else { it.to_str(&m)? };
        Ok(Value::Str(if m.is_empty() { n } else if n.is_empty() { m } else { format!("{n}: {m}").into() }))
    }, 0);
    let names: [(&str, fn(&mut Interp, &A) -> R<Value>); 6] = [
        ("TypeError", |it, a| {
            let p = it.p.type_error;
            make(it, a, p)
        }),
        ("RangeError", |it, a| {
            let p = it.p.range_error;
            make(it, a, p)
        }),
        ("SyntaxError", |it, a| {
            let p = it.p.syntax_error;
            make(it, a, p)
        }),
        ("ReferenceError", |it, a| {
            let p = it.p.reference_error;
            make(it, a, p)
        }),
        ("EvalError", |it, a| {
            let p = it.p.error;
            make(it, a, p)
        }),
        ("URIError", |it, a| {
            let p = it.p.error;
            make(it, a, p)
        }),
    ];
    for (name, f) in names {
        let p = it.alloc(Obj::new(Some(error), Kind::Plain));
        it.hide(p, "name", Value::str(name));
        it.hide(p, "message", Value::str(""));
        match name {
            "TypeError" => it.p.type_error = Some(p),
            "RangeError" => it.p.range_error = Some(p),
            "SyntaxError" => it.p.syntax_error = Some(p),
            "ReferenceError" => it.p.reference_error = Some(p),
            _ => {}
        }
        let c = ctor(it, name, f, 1, p);
        let ec = it.get(&Value::Obj(error), "constructor").ok().and_then(|v| v.as_obj());
        it.heap.get_mut(c).proto = ec;
    }
}

// ---- Math ------------------------------------------------------------------------------

fn random_u64() -> u64 {
    use std::cell::Cell;
    thread_local! {
        static STATE: Cell<u64> = Cell::new({
            use std::hash::{BuildHasher, Hasher};
            let mut h = std::collections::hash_map::RandomState::new().build_hasher();
            h.write_u64(0xA37E_5EED);
            h.finish() | 1
        });
    }
    STATE.with(|s| {
        // xorshift64*: for page scripts' Math.random, not for anything secret.
        let mut x = s.get();
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        s.set(x);
        x.wrapping_mul(0x2545F4914F6CDD1D)
    })
}

fn install_math(it: &mut Interp) {
    let m = it.new_object();
    let g = it.global;
    it.hide(g, "Math", Value::Obj(m));
    for (k, v) in [
        ("PI", std::f64::consts::PI),
        ("E", std::f64::consts::E),
        ("LN2", std::f64::consts::LN_2),
        ("LN10", std::f64::consts::LN_10),
        ("LOG2E", std::f64::consts::LOG2_E),
        ("LOG10E", std::f64::consts::LOG10_E),
        ("SQRT2", std::f64::consts::SQRT_2),
        ("SQRT1_2", std::f64::consts::FRAC_1_SQRT_2),
    ] {
        it.heap.get_mut(m).props.set(k.into(), Slot::Data(num(v), 0));
    }
    macro_rules! f1 {
        ($name:expr, $f:expr) => {
            it.method(m, $name, |it, a| {
                let x = it.to_num(&a.get(0))?;
                let f: fn(f64) -> f64 = $f;
                Ok(num(f(x)))
            }, 1);
        };
    }
    f1!("abs", f64::abs);
    f1!("floor", f64::floor);
    f1!("ceil", f64::ceil);
    f1!("round", |x| if x.is_finite() { (x + 0.5).floor() } else { x });
    f1!("trunc", f64::trunc);
    f1!("sign", |x| if x.is_nan() || x == 0.0 { x } else { x.signum() });
    f1!("sqrt", f64::sqrt);
    f1!("cbrt", f64::cbrt);
    f1!("sin", f64::sin);
    f1!("cos", f64::cos);
    f1!("tan", f64::tan);
    f1!("asin", f64::asin);
    f1!("acos", f64::acos);
    f1!("atan", f64::atan);
    f1!("sinh", f64::sinh);
    f1!("cosh", f64::cosh);
    f1!("tanh", f64::tanh);
    f1!("exp", f64::exp);
    f1!("expm1", f64::exp_m1);
    f1!("log", f64::ln);
    f1!("log2", f64::log2);
    f1!("log10", f64::log10);
    f1!("log1p", f64::ln_1p);
    f1!("fround", |x| x as f32 as f64);
    it.method(m, "clz32", |it, a| Ok(num((it.to_num(&a.get(0))? as i64 as u32).leading_zeros() as f64)), 1);
    it.method(m, "imul", |it, a| {
        let x = it.to_int32(&a.get(0))?;
        let y = it.to_int32(&a.get(1))?;
        Ok(num(x.wrapping_mul(y) as f64))
    }, 2);
    it.method(m, "atan2", |it, a| {
        let y = it.to_num(&a.get(0))?;
        let x = it.to_num(&a.get(1))?;
        Ok(num(y.atan2(x)))
    }, 2);
    it.method(m, "pow", |it, a| {
        let x = it.to_num(&a.get(0))?;
        let y = it.to_num(&a.get(1))?;
        it.binop(super::ast::BinOp::Exp, &num(x), &num(y))
    }, 2);
    it.method(m, "hypot", |it, a| {
        let mut s = 0.0;
        for x in a.args {
            let v = it.to_num(x)?;
            s += v * v;
        }
        Ok(num(s.sqrt()))
    }, 2);
    it.method(m, "max", |it, a| {
        let mut r = f64::NEG_INFINITY;
        for x in a.args {
            let v = it.to_num(x)?;
            if v.is_nan() {
                return Ok(num(f64::NAN));
            }
            if v > r || (v == 0.0 && r == 0.0 && !v.is_sign_negative()) {
                r = v;
            }
        }
        Ok(num(r))
    }, 2);
    it.method(m, "min", |it, a| {
        let mut r = f64::INFINITY;
        for x in a.args {
            let v = it.to_num(x)?;
            if v.is_nan() {
                return Ok(num(f64::NAN));
            }
            if v < r || (v == 0.0 && r == 0.0 && v.is_sign_negative()) {
                r = v;
            }
        }
        Ok(num(r))
    }, 2);
    it.method(m, "random", |_, _| Ok(num((random_u64() >> 11) as f64 / (1u64 << 53) as f64)), 0);
}

// ---- JSON ----------------------------------------------------------------------------------

struct JsonParser<'a> {
    s: &'a [char],
    i: usize,
    depth: usize,
}

impl JsonParser<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], ' ' | '\t' | '\n' | '\r') {
            self.i += 1;
        }
    }

    fn fail<T>(&self, it: &mut Interp) -> R<T> {
        let what = match self.s.get(self.i) {
            Some(c) => format!("Unexpected token '{c}' in JSON at position {}", self.i),
            None => "Unexpected end of JSON input".into(),
        };
        Err(it.err(ErrKind::Syntax, &what))
    }

    fn value(&mut self, it: &mut Interp) -> R<Value> {
        self.ws();
        self.depth += 1;
        if self.depth > 512 {
            return Err(it.err(ErrKind::Syntax, "JSON nested too deeply"));
        }
        let r = self.value_inner(it);
        self.depth -= 1;
        r
    }

    fn value_inner(&mut self, it: &mut Interp) -> R<Value> {
        it.tick()?;
        let Some(&c) = self.s.get(self.i) else { return self.fail(it) };
        match c {
            '{' => {
                self.i += 1;
                let o = it.new_object();
                self.ws();
                if self.s.get(self.i) == Some(&'}') {
                    self.i += 1;
                    return Ok(Value::Obj(o));
                }
                loop {
                    self.ws();
                    if self.s.get(self.i) != Some(&'"') {
                        return self.fail(it);
                    }
                    let k = self.string(it)?;
                    self.ws();
                    if self.s.get(self.i) != Some(&':') {
                        return self.fail(it);
                    }
                    self.i += 1;
                    let v = self.value(it)?;
                    if &*k == "__proto__" {
                        it.heap.get_mut(o).props.set(k, Slot::Data(v, F_ALL));
                    } else {
                        it.define(o, &k, v);
                    }
                    self.ws();
                    match self.s.get(self.i) {
                        Some(',') => self.i += 1,
                        Some('}') => {
                            self.i += 1;
                            return Ok(Value::Obj(o));
                        }
                        _ => return self.fail(it),
                    }
                }
            }
            '[' => {
                self.i += 1;
                let mut v = Vec::new();
                self.ws();
                if self.s.get(self.i) == Some(&']') {
                    self.i += 1;
                    return Ok(it.new_array(v));
                }
                loop {
                    v.push(self.value(it)?);
                    self.ws();
                    match self.s.get(self.i) {
                        Some(',') => self.i += 1,
                        Some(']') => {
                            self.i += 1;
                            return Ok(it.new_array(v));
                        }
                        _ => return self.fail(it),
                    }
                }
            }
            '"' => Ok(Value::Str(self.string(it)?)),
            't' | 'f' | 'n' => {
                for (w, v) in [("true", Value::Bool(true)), ("false", Value::Bool(false)), ("null", Value::Null)] {
                    let wc: Vec<char> = w.chars().collect();
                    if self.s.len() >= self.i + wc.len() && self.s[self.i..self.i + wc.len()] == wc[..] {
                        self.i += wc.len();
                        return Ok(v);
                    }
                }
                self.fail(it)
            }
            '-' | '0'..='9' => {
                let start = self.i;
                if self.s[self.i] == '-' {
                    self.i += 1;
                }
                let int_start = self.i;
                while self.i < self.s.len() && self.s[self.i].is_ascii_digit() {
                    self.i += 1;
                }
                if self.i == int_start || (self.s[int_start] == '0' && self.i - int_start > 1) {
                    return self.fail(it);
                }
                if self.s.get(self.i) == Some(&'.') {
                    self.i += 1;
                    let f = self.i;
                    while self.i < self.s.len() && self.s[self.i].is_ascii_digit() {
                        self.i += 1;
                    }
                    if f == self.i {
                        return self.fail(it);
                    }
                }
                if matches!(self.s.get(self.i), Some('e' | 'E')) {
                    self.i += 1;
                    if matches!(self.s.get(self.i), Some('+' | '-')) {
                        self.i += 1;
                    }
                    let f = self.i;
                    while self.i < self.s.len() && self.s[self.i].is_ascii_digit() {
                        self.i += 1;
                    }
                    if f == self.i {
                        return self.fail(it);
                    }
                }
                let t: String = self.s[start..self.i].iter().collect();
                Ok(num(t.parse().unwrap_or(f64::NAN)))
            }
            _ => self.fail(it),
        }
    }

    fn string(&mut self, it: &mut Interp) -> R<JsStr> {
        self.i += 1;
        let mut out = String::new();
        let mut pending_hi: Option<u32> = None;
        loop {
            let Some(&c) = self.s.get(self.i) else { return self.fail(it) };
            self.i += 1;
            match c {
                '"' => break,
                '\\' => {
                    let Some(&e) = self.s.get(self.i) else { return self.fail(it) };
                    self.i += 1;
                    let ch = match e {
                        '"' => '"',
                        '\\' => '\\',
                        '/' => '/',
                        'b' => '\u{8}',
                        'f' => '\u{C}',
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        'u' => {
                            if self.i + 4 > self.s.len() {
                                return self.fail(it);
                            }
                            let h: String = self.s[self.i..self.i + 4].iter().collect();
                            let Ok(v) = u32::from_str_radix(&h, 16) else { return self.fail(it) };
                            self.i += 4;
                            if (0xD800..0xDC00).contains(&v) {
                                pending_hi = Some(v);
                                continue;
                            }
                            if (0xDC00..0xE000).contains(&v) {
                                if let Some(hi) = pending_hi.take() {
                                    out.push(char::from_u32(0x10000 + ((hi - 0xD800) << 10) + (v - 0xDC00)).unwrap_or('\u{FFFD}'));
                                    continue;
                                }
                            }
                            char::from_u32(v).unwrap_or('\u{FFFD}')
                        }
                        _ => return self.fail(it),
                    };
                    if pending_hi.take().is_some() {
                        out.push('\u{FFFD}');
                    }
                    out.push(ch);
                    continue;
                }
                c if (c as u32) < 0x20 => {
                    self.i -= 1;
                    return self.fail(it);
                }
                c => {
                    if pending_hi.take().is_some() {
                        out.push('\u{FFFD}');
                    }
                    out.push(c)
                }
            }
        }
        if pending_hi.is_some() {
            out.push('\u{FFFD}');
        }
        Ok(out.into())
    }
}

pub fn json_quote(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            '\u{8}' => o.push_str("\\b"),
            '\u{C}' => o.push_str("\\f"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

struct Stringify {
    replacer: Value,
    allow: Option<Vec<JsStr>>,
    gap: String,
    stack: Vec<Gc>,
}

fn stringify(it: &mut Interp, st: &mut Stringify, holder: &Value, key: &JsStr, indent: &str) -> R<Option<String>> {
    let mut v = it.get(holder, key)?;
    if let Value::Obj(_) = v {
        let tj = it.get(&v, "toJSON")?;
        if it.is_callable(&tj) {
            v = it.call(&tj, v.clone(), vec![Value::Str(key.clone())])?;
        }
    }
    if it.is_callable(&st.replacer) {
        let r = st.replacer.clone();
        v = it.call(&r, holder.clone(), vec![Value::Str(key.clone()), v])?;
    }
    if let Value::Obj(g) = &v {
        if let Kind::Boxed(b) = &it.heap.get(*g).kind {
            v = b.clone();
        }
    }
    it.tick()?;
    Ok(Some(match &v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Num(n) => {
            if n.is_finite() {
                num_to_str(*n)
            } else {
                "null".into()
            }
        }
        Value::Str(s) => json_quote(s),
        Value::Undef => return Ok(None),
        Value::Obj(g) => {
            if it.heap.get(*g).is_callable() {
                return Ok(None);
            }
            if st.stack.contains(g) {
                return it.type_err("Converting circular structure to JSON");
            }
            if st.stack.len() > 512 {
                return Err(it.err(ErrKind::Range, "Maximum call stack size exceeded"));
            }
            st.stack.push(*g);
            let inner = format!("{indent}{}", st.gap);
            let sep = if st.gap.is_empty() { "," .to_string() } else { format!(",\n{inner}") };
            let is_array = matches!(it.heap.get(*g).kind, Kind::Array(_));
            let out = if is_array {
                let n = array_like(it, &v)?.len();
                let mut parts = Vec::new();
                for i in 0..n {
                    let k: JsStr = i.to_string().into();
                    parts.push(stringify(it, st, &v, &k, &inner)?.unwrap_or_else(|| "null".into()));
                }
                if parts.is_empty() {
                    "[]".into()
                } else if st.gap.is_empty() {
                    format!("[{}]", parts.join(","))
                } else {
                    format!("[\n{inner}{}\n{indent}]", parts.join(&sep))
                }
            } else {
                let keys = match &st.allow {
                    Some(a) => a.clone(),
                    None => it.own_keys(*g, true)?,
                };
                let mut parts = Vec::new();
                for k in keys {
                    if let Some(s) = stringify(it, st, &v, &k, &inner)? {
                        let colon = if st.gap.is_empty() { ":" } else { ": " };
                        parts.push(format!("{}{colon}{s}", json_quote(&k)));
                    }
                }
                if parts.is_empty() {
                    "{}".into()
                } else if st.gap.is_empty() {
                    format!("{{{}}}", parts.join(","))
                } else {
                    format!("{{\n{inner}{}\n{indent}}}", parts.join(&sep))
                }
            };
            st.stack.pop();
            if out.len() > MAX_STRING {
                return Err(it.err(ErrKind::Range, "Invalid string length"));
            }
            out
        }
    }))
}

fn install_json(it: &mut Interp) {
    let j = it.new_object();
    let g = it.global;
    it.hide(g, "JSON", Value::Obj(j));
    it.method(j, "parse", |it, a| {
        let s = it.to_str(&a.get(0))?;
        let c: Vec<char> = s.chars().collect();
        let mut p = JsonParser { s: &c, i: 0, depth: 0 };
        let v = p.value(it)?;
        p.ws();
        if p.i != c.len() {
            return p.fail(it);
        }
        let reviver = a.get(1);
        if it.is_callable(&reviver) {
            let root = it.new_object();
            it.define(root, "", v);
            return revive(it, &Value::Obj(root), "".into(), &reviver, 0);
        }
        Ok(v)
    }, 2);
    it.method(j, "stringify", |it, a| {
        let replacer = a.get(1);
        let allow = match &replacer {
            Value::Obj(g) if matches!(it.heap.get(*g).kind, Kind::Array(_)) => {
                let items = array_like(it, &replacer)?;
                let mut v = Vec::new();
                for x in items {
                    if matches!(x, Value::Str(_) | Value::Num(_)) {
                        v.push(it.to_str(&x)?);
                    }
                }
                Some(v)
            }
            _ => None,
        };
        let gap = match a.get(2) {
            Value::Num(n) => " ".repeat((n.max(0.0) as usize).min(10)),
            Value::Str(s) => s.chars().take(10).collect(),
            _ => String::new(),
        };
        let mut st = Stringify { replacer, allow, gap, stack: Vec::new() };
        let root = it.new_object();
        it.define(root, "", a.get(0));
        Ok(match stringify(it, &mut st, &Value::Obj(root), &"".into(), "")? {
            Some(s) => Value::Str(s.into()),
            None => Value::Undef,
        })
    }, 3);
}

fn revive(it: &mut Interp, holder: &Value, key: JsStr, f: &Value, depth: usize) -> R<Value> {
    let v = it.get(holder, &key)?;
    if let (Value::Obj(g), true) = (&v, depth < 512) {
        for k in it.own_keys(*g, true)? {
            let nv = revive(it, &v, k.clone(), f, depth + 1)?;
            if matches!(nv, Value::Undef) {
                it.delete(&v, &k)?;
            } else {
                it.define(*g, &k, nv);
            }
        }
    }
    it.call(f, holder.clone(), vec![Value::Str(key), v])
}

// ---- RegExp ----------------------------------------------------------------------------------

fn install_regexp(it: &mut Interp) {
    let proto = it.p.regexp.unwrap();
    ctor(
        it,
        "RegExp",
        |it, a| {
            let (src, flags) = match (regexp_of(it, &a.get(0)), a.get(1)) {
                (Some(r), Value::Undef) => (JsStr::from(r.source.as_str()), JsStr::from(r.flags.as_str())),
                (Some(r), f) => (JsStr::from(r.source.as_str()), it.to_str(&f)?),
                (None, f) => {
                    let s = match a.get(0) {
                        Value::Undef => "(?:)".into(),
                        v => it.to_str(&v)?,
                    };
                    let f = if matches!(f, Value::Undef) { "".into() } else { it.to_str(&f)? };
                    (s, f)
                }
            };
            let re = it.regex(&src, &flags)?;
            Ok(it.new_regexp(re))
        },
        2,
        proto,
    );
    it.method(proto, "exec", |it, a| {
        let s = it.to_str(&a.get(0))?;
        regexp_exec(it, &a.this, &s)
    }, 1);
    it.method(proto, "test", |it, a| {
        let s = it.to_str(&a.get(0))?;
        Ok(Value::Bool(!matches!(regexp_exec(it, &a.this, &s)?, Value::Null)))
    }, 1);
    it.method(proto, "toString", |it, a| match regexp_of(it, &a.this) {
        Some(r) => Ok(Value::Str(format!("/{}/{}", r.source, r.flags).into())),
        None => Ok(Value::str("/(?:)/")),
    }, 0);
    it.getter(proto, "source", |it, a| Ok(Value::Str(regexp_of(it, &a.this).map(|r| r.source.clone()).unwrap_or_else(|| "(?:)".into()).into())));
    it.getter(proto, "flags", |it, a| Ok(Value::Str(regexp_of(it, &a.this).map(|r| r.flags.clone()).unwrap_or_default().into())));
    it.getter(proto, "global", |it, a| Ok(Value::Bool(regexp_of(it, &a.this).is_some_and(|r| r.global))));
    it.getter(proto, "ignoreCase", |it, a| Ok(Value::Bool(regexp_of(it, &a.this).is_some_and(|r| r.ignore_case))));
    it.getter(proto, "multiline", |it, a| Ok(Value::Bool(regexp_of(it, &a.this).is_some_and(|r| r.multiline))));
    it.getter(proto, "sticky", |it, a| Ok(Value::Bool(regexp_of(it, &a.this).is_some_and(|r| r.sticky))));
}

// ---- Date ------------------------------------------------------------------------------------

pub fn now_ms() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as f64).unwrap_or(0.0)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// MakeDate from (possibly overflowing) fields; month is 0-based.
fn make_time(y: f64, mo: f64, d: f64, h: f64, mi: f64, s: f64, ms: f64) -> f64 {
    if ![y, mo, d, h, mi, s, ms].iter().all(|x| x.is_finite()) {
        return f64::NAN;
    }
    let ym = y + (mo / 12.0).floor();
    let mn = mo.rem_euclid(12.0);
    if ym.abs() > 400_000.0 {
        return f64::NAN;
    }
    let days = days_from_civil(ym as i64, mn as i64 + 1, 1) as f64 + d - 1.0;
    let t = days * 86_400_000.0 + h * 3_600_000.0 + mi * 60_000.0 + s * 1000.0 + ms;
    if t.abs() > 8.64e15 {
        f64::NAN
    } else {
        t.trunc()
    }
}

struct Fields {
    y: i64,
    mo: i64,
    d: i64,
    h: i64,
    mi: i64,
    s: i64,
    ms: i64,
    wd: i64,
}

fn fields(t: f64) -> Fields {
    let t = t as i64;
    let days = t.div_euclid(86_400_000);
    let rem = t.rem_euclid(86_400_000);
    let (y, m, d) = civil_from_days(days);
    Fields { y, mo: m - 1, d, h: rem / 3_600_000, mi: rem / 60_000 % 60, s: rem / 1000 % 60, ms: rem % 1000, wd: (days + 4).rem_euclid(7) }
}

/// Date.parse: ISO 8601 forms and the common "Mon, 01 Jan 2024 10:00:00 GMT".
pub fn parse_date(s: &str) -> f64 {
    let s = s.trim();
    let b: Vec<char> = s.chars().collect();
    let num = |from: usize, n: usize| -> Option<f64> {
        if from + n > b.len() || !b[from..from + n].iter().all(|c| c.is_ascii_digit()) {
            return None;
        }
        b[from..from + n].iter().collect::<String>().parse().ok()
    };
    if let Some(y) = num(0, 4) {
        let mut mo = 1.0;
        let mut d = 1.0;
        let (mut h, mut mi, mut sec, mut ms) = (0.0, 0.0, 0.0, 0.0);
        let mut i = 4;
        if b.get(i) == Some(&'-') {
            mo = match num(i + 1, 2) {
                Some(v) => v,
                None => return f64::NAN,
            };
            i += 3;
            if b.get(i) == Some(&'-') {
                d = match num(i + 1, 2) {
                    Some(v) => v,
                    None => return f64::NAN,
                };
                i += 3;
            }
        }
        let mut offset = 0.0;
        if matches!(b.get(i), Some('T' | ' ')) {
            h = num(i + 1, 2).unwrap_or(f64::NAN);
            mi = if b.get(i + 3) == Some(&':') { num(i + 4, 2).unwrap_or(f64::NAN) } else { f64::NAN };
            i += 6;
            if b.get(i) == Some(&':') {
                sec = num(i + 1, 2).unwrap_or(f64::NAN);
                i += 3;
                if b.get(i) == Some(&'.') {
                    let start = i + 1;
                    let mut j = start;
                    while j < b.len() && b[j].is_ascii_digit() {
                        j += 1;
                    }
                    let frac: String = b[start..j].iter().take(3).collect();
                    ms = format!("{frac:0<3}").parse().unwrap_or(0.0);
                    i = j;
                }
            }
            match b.get(i) {
                Some('Z') => i += 1,
                Some(&c) if c == '+' || c == '-' => {
                    let oh = num(i + 1, 2).unwrap_or(0.0);
                    let om = if b.get(i + 3) == Some(&':') { num(i + 4, 2).unwrap_or(0.0) } else { num(i + 3, 2).unwrap_or(0.0) };
                    offset = (oh * 60.0 + om) * if c == '+' { 1.0 } else { -1.0 };
                    i = b.len();
                }
                _ => {}
            }
        }
        if i < b.len() {
            return f64::NAN;
        }
        return make_time(y, mo - 1.0, d, h, mi, sec, ms) - offset * 60_000.0;
    }
    // RFC 2822-ish: [Day,] DD Mon YYYY [HH:MM[:SS]] [GMT|+hhmm]
    const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
    let words: Vec<String> = s.replace(',', " ").split_whitespace().map(|w| w.to_ascii_lowercase()).collect();
    let mut day = None;
    let mut mon = None;
    let mut year = None;
    let mut time = (0.0, 0.0, 0.0);
    let mut off = 0.0;
    for w in &words {
        if let Some(m) = MONTHS.iter().position(|m| w.starts_with(m)) {
            mon = Some(m as f64);
        } else if w.contains(':') {
            let p: Vec<f64> = w.split(':').map(|x| x.parse().unwrap_or(f64::NAN)).collect();
            time = (p.first().copied().unwrap_or(0.0), p.get(1).copied().unwrap_or(0.0), p.get(2).copied().unwrap_or(0.0));
        } else if let Ok(n) = w.parse::<f64>() {
            if n > 31.0 || day.is_some() {
                year = Some(n);
            } else {
                day = Some(n);
            }
        } else if (w.starts_with('+') || w.starts_with('-')) && w.len() == 5 {
            let n: f64 = w[1..].parse().unwrap_or(0.0);
            off = ((n / 100.0).trunc() * 60.0 + n % 100.0) * if w.starts_with('+') { 1.0 } else { -1.0 };
        }
    }
    match (day, mon, year) {
        (Some(d), Some(m), Some(y)) => make_time(y, m, d, time.0, time.1, time.2, 0.0) - off * 60_000.0,
        _ => f64::NAN,
    }
}

fn this_date(it: &mut Interp, a: &A) -> R<f64> {
    match &a.this {
        Value::Obj(g) => match &it.heap.get(*g).kind {
            Kind::Date(t) => Ok(*t),
            _ => it.type_err("this is not a Date object."),
        },
        _ => it.type_err("this is not a Date object."),
    }
}

const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

fn install_date(it: &mut Interp) {
    let proto = it.p.date.unwrap();
    let c = ctor(
        it,
        "Date",
        |it, a| {
            if a.new_target.is_none() {
                let f = fields(now_ms());
                return Ok(Value::Str(format!("{} {} {:02} {} {:02}:{:02}:{:02} GMT+0000 (Coordinated Universal Time)", DAYS[f.wd as usize], MONS[f.mo as usize], f.d, f.y, f.h, f.mi, f.s).into()));
            }
            let t = match a.args.len() {
                0 => now_ms(),
                1 => match a.get(0) {
                    Value::Obj(g) if matches!(it.heap.get(g).kind, Kind::Date(_)) => match it.heap.get(g).kind {
                        Kind::Date(t) => t,
                        _ => f64::NAN,
                    },
                    v => {
                        let p = it.to_primitive(&v, false)?;
                        match p {
                            Value::Str(s) => parse_date(&s),
                            v => {
                                let n = it.to_num(&v)?;
                                if n.abs() > 8.64e15 {
                                    f64::NAN
                                } else {
                                    n.trunc()
                                }
                            }
                        }
                    }
                },
                _ => {
                    let mut f = [0.0f64, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0];
                    for (i, v) in a.args.iter().take(7).enumerate() {
                        f[i] = it.to_num(v)?;
                    }
                    if (0.0..=99.0).contains(&f[0]) {
                        f[0] += 1900.0;
                    }
                    make_time(f[0], f[1], f[2], f[3], f[4], f[5], f[6])
                }
            };
            let p = match &a.new_target {
                Some(nt) => match it.get(nt, "prototype")? {
                    Value::Obj(p) => Some(p),
                    _ => it.p.date,
                },
                None => it.p.date,
            };
            Ok(Value::Obj(it.alloc(Obj::new(p, Kind::Date(t)))))
        },
        7,
        proto,
    );
    it.method(c, "now", |_, _| Ok(num(now_ms())), 0);
    it.method(c, "parse", |it, a| {
        let s = it.to_str(&a.get(0))?;
        Ok(num(parse_date(&s)))
    }, 1);
    it.method(c, "UTC", |it, a| {
        let mut f = [0.0f64, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0];
        for (i, v) in a.args.iter().take(7).enumerate() {
            f[i] = it.to_num(v)?;
        }
        if (0.0..=99.0).contains(&f[0]) {
            f[0] += 1900.0;
        }
        Ok(num(make_time(f[0], f[1], f[2], f[3], f[4], f[5], f[6])))
    }, 7);
    macro_rules! get {
        ($($name:expr),* ; $f:expr) => {
            $(
                it.method(proto, $name, |it, a| {
                    let t = this_date(it, a)?;
                    if t.is_nan() {
                        return Ok(num(f64::NAN));
                    }
                    let f = fields(t);
                    let g: fn(&Fields) -> i64 = $f;
                    Ok(num(g(&f) as f64))
                }, 0);
            )*
        };
    }
    get!("getFullYear", "getUTCFullYear"; |f| f.y);
    get!("getMonth", "getUTCMonth"; |f| f.mo);
    get!("getDate", "getUTCDate"; |f| f.d);
    get!("getDay", "getUTCDay"; |f| f.wd);
    get!("getHours", "getUTCHours"; |f| f.h);
    get!("getMinutes", "getUTCMinutes"; |f| f.mi);
    get!("getSeconds", "getUTCSeconds"; |f| f.s);
    get!("getMilliseconds", "getUTCMilliseconds"; |f| f.ms);
    get!("getYear"; |f| f.y - 1900);
    it.method(proto, "getTime", |it, a| Ok(num(this_date(it, a)?)), 0);
    it.method(proto, "valueOf", |it, a| Ok(num(this_date(it, a)?)), 0);
    it.method(proto, "getTimezoneOffset", |it, a| {
        this_date(it, a)?;
        Ok(num(0.0))
    }, 0);
    fn set_fields(it: &mut Interp, a: &A, first: usize) -> R<Value> {
        let t = this_date(it, a)?;
        let f = if t.is_nan() { fields(0.0) } else { fields(t) };
        let mut v = [f.y as f64, f.mo as f64, f.d as f64, f.h as f64, f.mi as f64, f.s as f64, f.ms as f64];
        let max = match first {
            0 => 3,
            1 => 2,
            2 => 1,
            3 => 4,
            4 => 3,
            5 => 2,
            _ => 1,
        };
        for (k, x) in a.args.iter().take(max).enumerate() {
            v[first + k] = it.to_num(x)?;
        }
        let nt = if t.is_nan() && first != 0 { f64::NAN } else { make_time(v[0], v[1], v[2], v[3], v[4], v[5], v[6]) };
        if let Value::Obj(g) = &a.this {
            it.heap.get_mut(*g).kind = Kind::Date(nt);
        }
        Ok(num(nt))
    }
    for (names, first) in [
        (["setFullYear", "setUTCFullYear"], 0usize),
        (["setMonth", "setUTCMonth"], 1),
        (["setDate", "setUTCDate"], 2),
        (["setHours", "setUTCHours"], 3),
        (["setMinutes", "setUTCMinutes"], 4),
        (["setSeconds", "setUTCSeconds"], 5),
        (["setMilliseconds", "setUTCMilliseconds"], 6),
    ] {
        let f: NativeFn = match first {
            0 => |it, a| set_fields(it, a, 0),
            1 => |it, a| set_fields(it, a, 1),
            2 => |it, a| set_fields(it, a, 2),
            3 => |it, a| set_fields(it, a, 3),
            4 => |it, a| set_fields(it, a, 4),
            5 => |it, a| set_fields(it, a, 5),
            _ => |it, a| set_fields(it, a, 6),
        };
        for n in names {
            it.method(proto, n, f, 1);
        }
    }
    it.method(proto, "setTime", |it, a| {
        this_date(it, a)?;
        let t = it.to_num(&a.get(0))?;
        let t = if t.abs() > 8.64e15 { f64::NAN } else { t.trunc() };
        if let Value::Obj(g) = &a.this {
            it.heap.get_mut(*g).kind = Kind::Date(t);
        }
        Ok(num(t))
    }, 1);
    fn iso(t: f64) -> Option<String> {
        if t.is_nan() {
            return None;
        }
        let f = fields(t);
        let y = if (0..=9999).contains(&f.y) { format!("{:04}", f.y) } else { format!("{}{:06}", if f.y < 0 { "-" } else { "+" }, f.y.abs()) };
        Some(format!("{y}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z", f.mo + 1, f.d, f.h, f.mi, f.s, f.ms))
    }
    it.method(proto, "toISOString", |it, a| {
        let t = this_date(it, a)?;
        match iso(t) {
            Some(s) => Ok(Value::Str(s.into())),
            None => Err(it.err(ErrKind::Range, "Invalid time value")),
        }
    }, 0);
    it.method(proto, "toJSON", |it, a| {
        let t = this_date(it, a)?;
        Ok(iso(t).map(|s| Value::Str(s.into())).unwrap_or(Value::Null))
    }, 1);
    fn fmt(it: &mut Interp, a: &A, kind: u8) -> R<Value> {
        let t = this_date(it, a)?;
        if t.is_nan() {
            return Ok(Value::str("Invalid Date"));
        }
        let f = fields(t);
        let date = format!("{} {} {:02} {}", DAYS[f.wd as usize], MONS[f.mo as usize], f.d, f.y);
        let time = format!("{:02}:{:02}:{:02} GMT+0000 (Coordinated Universal Time)", f.h, f.mi, f.s);
        Ok(Value::Str(
            match kind {
                0 => format!("{date} {time}"),
                1 => date,
                2 => time,
                3 => format!("{}/{}/{}", f.mo + 1, f.d, f.y),
                4 => {
                    let h12 = if f.h % 12 == 0 { 12 } else { f.h % 12 };
                    format!("{h12}:{:02}:{:02} {}", f.mi, f.s, if f.h < 12 { "AM" } else { "PM" })
                }
                5 => {
                    let h12 = if f.h % 12 == 0 { 12 } else { f.h % 12 };
                    format!("{}/{}/{}, {h12}:{:02}:{:02} {}", f.mo + 1, f.d, f.y, f.mi, f.s, if f.h < 12 { "AM" } else { "PM" })
                }
                _ => format!("{}, {:02} {} {} {:02}:{:02}:{:02} GMT", DAYS[f.wd as usize], f.d, MONS[f.mo as usize], f.y, f.h, f.mi, f.s),
            }
            .into(),
        ))
    }
    it.method(proto, "toString", |it, a| fmt(it, a, 0), 0);
    it.method(proto, "toDateString", |it, a| fmt(it, a, 1), 0);
    it.method(proto, "toTimeString", |it, a| fmt(it, a, 2), 0);
    it.method(proto, "toLocaleDateString", |it, a| fmt(it, a, 3), 0);
    it.method(proto, "toLocaleTimeString", |it, a| fmt(it, a, 4), 0);
    it.method(proto, "toLocaleString", |it, a| fmt(it, a, 5), 0);
    it.method(proto, "toUTCString", |it, a| fmt(it, a, 6), 0);
    it.method(proto, "toGMTString", |it, a| fmt(it, a, 6), 0);
}

// ---- Map and Set --------------------------------------------------------------------------------

fn this_map(it: &mut Interp, a: &A, set: bool) -> R<Gc> {
    match &a.this {
        Value::Obj(g) if (set && matches!(it.heap.get(*g).kind, Kind::Set(_))) || (!set && matches!(it.heap.get(*g).kind, Kind::Map(_))) => Ok(*g),
        _ => it.type_err(if set { "Set method called on incompatible receiver" } else { "Map method called on incompatible receiver" }),
    }
}

fn map_mut(it: &mut Interp, g: Gc) -> &mut OrderedMap {
    match &mut it.heap.get_mut(g).kind {
        Kind::Map(m) | Kind::Set(m) => m,
        _ => unreachable!(),
    }
}

fn map_ref(it: &Interp, g: Gc) -> &OrderedMap {
    match &it.heap.get(g).kind {
        Kind::Map(m) | Kind::Set(m) => m,
        _ => unreachable!(),
    }
}

fn install_collections(it: &mut Interp) {
    for set in [false, true] {
        let proto = if set { it.p.set.unwrap() } else { it.p.map.unwrap() };
        let f: NativeFn = if set {
            |it, a| {
                if a.new_target.is_none() {
                    return it.type_err("Constructor Set requires 'new'");
                }
                let p = it.p.set;
                let g = it.alloc(Obj::new(p, Kind::Set(OrderedMap::default())));
                if !a.get(0).is_nullish() {
                    for v in it.collect(&a.get(0))? {
                        map_mut(it, g).set(v, Value::Undef);
                    }
                }
                Ok(Value::Obj(g))
            }
        } else {
            |it, a| {
                if a.new_target.is_none() {
                    return it.type_err("Constructor Map requires 'new'");
                }
                let p = it.p.map;
                let g = it.alloc(Obj::new(p, Kind::Map(OrderedMap::default())));
                if !a.get(0).is_nullish() {
                    for e in it.collect(&a.get(0))? {
                        let k = it.get_v(&e, &num(0.0))?;
                        let v = it.get_v(&e, &num(1.0))?;
                        map_mut(it, g).set(k, v);
                    }
                }
                Ok(Value::Obj(g))
            }
        };
        let name = if set { "Set" } else { "Map" };
        ctor(it, name, f, 0, proto);
        // WeakMap/WeakSet: the same, holding strongly (the page's heap is dropped whole anyway).
        let weak = if set { "WeakSet" } else { "WeakMap" };
        let c = ctor(it, weak, f, 0, proto);
        let _ = c;
        if set {
            it.method(proto, "add", |it, a| {
                let g = this_map(it, a, true)?;
                map_mut(it, g).set(a.get(0), Value::Undef);
                Ok(a.this.clone())
            }, 1);
        } else {
            it.method(proto, "get", |it, a| {
                let g = this_map(it, a, false)?;
                Ok(map_ref(it, g).get(&a.get(0)).cloned().unwrap_or(Value::Undef))
            }, 1);
            it.method(proto, "set", |it, a| {
                let g = this_map(it, a, false)?;
                map_mut(it, g).set(a.get(0), a.get(1));
                Ok(a.this.clone())
            }, 2);
        }
        let has: NativeFn = if set {
            |it, a| {
                let g = this_map(it, a, true)?;
                Ok(Value::Bool(map_ref(it, g).get(&a.get(0)).is_some()))
            }
        } else {
            |it, a| {
                let g = this_map(it, a, false)?;
                Ok(Value::Bool(map_ref(it, g).get(&a.get(0)).is_some()))
            }
        };
        it.method(proto, "has", has, 1);
        let del: NativeFn = if set {
            |it, a| {
                let g = this_map(it, a, true)?;
                Ok(Value::Bool(map_mut(it, g).delete(&a.get(0))))
            }
        } else {
            |it, a| {
                let g = this_map(it, a, false)?;
                Ok(Value::Bool(map_mut(it, g).delete(&a.get(0))))
            }
        };
        it.method(proto, "delete", del, 1);
        let clear: NativeFn = if set {
            |it, a| {
                let g = this_map(it, a, true)?;
                map_mut(it, g).clear();
                Ok(Value::Undef)
            }
        } else {
            |it, a| {
                let g = this_map(it, a, false)?;
                map_mut(it, g).clear();
                Ok(Value::Undef)
            }
        };
        it.method(proto, "clear", clear, 0);
        let size: NativeFn = if set {
            |it, a| {
                let g = this_map(it, a, true)?;
                Ok(num(map_ref(it, g).len() as f64))
            }
        } else {
            |it, a| {
                let g = this_map(it, a, false)?;
                Ok(num(map_ref(it, g).len() as f64))
            }
        };
        it.getter(proto, "size", size);
        let for_each: NativeFn = |it, a| {
            let Value::Obj(g) = a.this.clone() else { return it.type_err("forEach on a non-collection") };
            let is_set = matches!(it.heap.get(g).kind, Kind::Set(_));
            if !is_set && !matches!(it.heap.get(g).kind, Kind::Map(_)) {
                return it.type_err("forEach on a non-collection");
            }
            let f = callback(it, a)?;
            let mut i = 0;
            loop {
                let e = {
                    let m = map_ref(it, g);
                    if i >= m.entries.len() {
                        break;
                    }
                    m.entries[i].clone()
                };
                if let Some((k, v)) = e {
                    let v = if is_set { k.clone() } else { v };
                    it.call(&f, a.get(1), vec![v, k, a.this.clone()])?;
                }
                i += 1;
            }
            Ok(Value::Undef)
        };
        it.method(proto, "forEach", for_each, 1);
        it.method(proto, "keys", |it, a| make_iter(it, a.this.clone(), IterKind::Keys), 0);
        let values = it.method(proto, "values", |it, a| make_iter(it, a.this.clone(), IterKind::Values), 0);
        let entries = it.method(proto, "entries", |it, a| make_iter(it, a.this.clone(), IterKind::Entries), 0);
        it.hide(proto, "@@iterator", Value::Obj(if set { values } else { entries }));
    }
}

// ---- Promise ------------------------------------------------------------------------------

pub fn new_promise(it: &mut Interp) -> Gc {
    let p = it.p.promise;
    it.alloc(Obj::new(p, Kind::Promise(Promise { state: PState::Pending, reactions: Vec::new(), handled: false })))
}

pub fn promise_state(it: &Interp, p: Gc) -> Option<PState> {
    match &it.heap.get(p).kind {
        Kind::Promise(pr) => Some(pr.state.clone()),
        _ => None,
    }
}

/// Fulfils or rejects `p` (once) and queues its reactions.
pub fn settle(it: &mut Interp, p: Gc, state: PState) {
    let reactions = match &mut it.heap.get_mut(p).kind {
        Kind::Promise(pr) if matches!(pr.state, PState::Pending) => {
            pr.state = state.clone();
            std::mem::take(&mut pr.reactions)
        }
        _ => return,
    };
    if let PState::Rejected(_) = &state {
        let handled = matches!(&it.heap.get(p).kind, Kind::Promise(pr) if pr.handled);
        if !handled && reactions.is_empty() {
            it.unhandled.push(p);
        }
    }
    for (ok, bad, child) in reactions {
        queue_reaction(it, &state, ok, bad, child);
    }
}

fn queue_reaction(it: &mut Interp, state: &PState, ok: Value, bad: Value, child: Option<Gc>) {
    let (handler, arg, rejected) = match state {
        PState::Fulfilled(v) => (ok, v.clone(), false),
        PState::Rejected(e) => (bad, e.clone(), true),
        PState::Pending => return,
    };
    let job = it.new_native_data("", reaction_job, 1, vec![handler, child.map(Value::Obj).unwrap_or(Value::Undef), Value::Bool(rejected)]);
    it.microtasks.push_back((Value::Obj(job), vec![arg]));
}

fn reaction_job(it: &mut Interp, a: &A) -> R<Value> {
    let data = match &it.heap.get(a.callee).kind {
        Kind::Native(n) => n.data.clone(),
        _ => return Ok(Value::Undef),
    };
    let (handler, child, rejected) = (data[0].clone(), data[1].as_obj(), matches!(data[2], Value::Bool(true)));
    let arg = a.get(0);
    if it.is_callable(&handler) {
        match it.call(&handler, Value::Undef, vec![arg]) {
            Ok(v) => {
                if let Some(c) = child {
                    resolve_promise(it, c, v)?;
                }
            }
            Err(Ctrl::Throw(e)) => {
                if let Some(c) = child {
                    settle(it, c, PState::Rejected(e));
                }
            }
            Err(e) => return Err(e),
        }
    } else if let Some(c) = child {
        if rejected {
            settle(it, c, PState::Rejected(arg));
        } else {
            resolve_promise(it, c, arg)?;
        }
    }
    Ok(Value::Undef)
}

/// The resolve and reject functions handed to an executor (sharing one "already resolved" flag).
fn resolving_functions(it: &mut Interp, p: Gc) -> (Value, Value) {
    let flag = it.alloc(Obj::new(None, Kind::Boxed(Value::Bool(false))));
    let res = it.new_native_data("resolve", |it, a| {
        let data = match &it.heap.get(a.callee).kind {
            Kind::Native(n) => n.data.clone(),
            _ => return Ok(Value::Undef),
        };
        let (p, flag) = (data[0].as_obj().unwrap(), data[1].as_obj().unwrap());
        if matches!(it.heap.get(flag).kind, Kind::Boxed(Value::Bool(true))) {
            return Ok(Value::Undef);
        }
        it.heap.get_mut(flag).kind = Kind::Boxed(Value::Bool(true));
        resolve_promise(it, p, a.get(0))?;
        Ok(Value::Undef)
    }, 1, vec![Value::Obj(p), Value::Obj(flag)]);
    let rej = it.new_native_data("reject", |it, a| {
        let data = match &it.heap.get(a.callee).kind {
            Kind::Native(n) => n.data.clone(),
            _ => return Ok(Value::Undef),
        };
        let (p, flag) = (data[0].as_obj().unwrap(), data[1].as_obj().unwrap());
        if matches!(it.heap.get(flag).kind, Kind::Boxed(Value::Bool(true))) {
            return Ok(Value::Undef);
        }
        it.heap.get_mut(flag).kind = Kind::Boxed(Value::Bool(true));
        settle(it, p, PState::Rejected(a.get(0)));
        Ok(Value::Undef)
    }, 1, vec![Value::Obj(p), Value::Obj(flag)]);
    (Value::Obj(res), Value::Obj(rej))
}

pub fn resolve_promise(it: &mut Interp, p: Gc, v: Value) -> R<()> {
    if let Value::Obj(g) = &v {
        if *g == p {
            let e = it.make_error(ErrKind::Type, "Chaining cycle detected for promise");
            settle(it, p, PState::Rejected(e));
            return Ok(());
        }
        let then = match it.get(&v, "then") {
            Ok(t) => t,
            Err(Ctrl::Throw(e)) => {
                settle(it, p, PState::Rejected(e));
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        if it.is_callable(&then) {
            let (res, rej) = resolving_functions(it, p);
            let job = it.new_native_data("", |it, a| {
                let data = match &it.heap.get(a.callee).kind {
                    Kind::Native(n) => n.data.clone(),
                    _ => return Ok(Value::Undef),
                };
                match it.call(&data[1], data[0].clone(), vec![data[2].clone(), data[3].clone()]) {
                    Ok(_) => {}
                    Err(Ctrl::Throw(e)) => {
                        let _ = it.call(&data[3], Value::Undef, vec![e]);
                    }
                    Err(e) => return Err(e),
                }
                Ok(Value::Undef)
            }, 0, vec![v.clone(), then, res, rej]);
            it.microtasks.push_back((Value::Obj(job), Vec::new()));
            return Ok(());
        }
    }
    settle(it, p, PState::Fulfilled(v));
    Ok(())
}

pub fn promise_then(it: &mut Interp, p: Gc, ok: Value, bad: Value) -> Gc {
    let child = new_promise(it);
    let state = match &mut it.heap.get_mut(p).kind {
        Kind::Promise(pr) => {
            pr.handled = true;
            if matches!(pr.state, PState::Pending) {
                pr.reactions.push((ok.clone(), bad.clone(), Some(child)));
                None
            } else {
                Some(pr.state.clone())
            }
        }
        _ => None,
    };
    it.unhandled.retain(|x| *x != p);
    if let Some(s) = state {
        queue_reaction(it, &s, ok, bad, Some(child));
    }
    child
}

/// Promise.resolve(v).
pub fn promise_resolved(it: &mut Interp, v: Value) -> R<Value> {
    if let Value::Obj(g) = &v {
        if matches!(it.heap.get(*g).kind, Kind::Promise(_)) {
            return Ok(v);
        }
    }
    let p = new_promise(it);
    resolve_promise(it, p, v)?;
    Ok(Value::Obj(p))
}

pub fn promise_rejected(it: &mut Interp, e: Value) -> Value {
    let p = new_promise(it);
    settle(it, p, PState::Rejected(e));
    Value::Obj(p)
}

/// `await v`: runs queued work until the promise settles (see the module notes in script/mod.rs).
pub fn await_value(it: &mut Interp, v: Value) -> R<Value> {
    let p = promise_resolved(it, v)?;
    let Value::Obj(pg) = p else { unreachable!() };
    if let Kind::Promise(pr) = &mut it.heap.get_mut(pg).kind {
        pr.handled = true;
    }
    it.unhandled.retain(|x| *x != pg);
    loop {
        match promise_state(it, pg) {
            Some(PState::Fulfilled(v)) => return Ok(v),
            Some(PState::Rejected(e)) => return Err(Ctrl::Throw(e)),
            _ => {}
        }
        if let Some((f, args)) = it.microtasks.pop_front() {
            match it.call(&f, Value::Undef, args) {
                Ok(_) | Err(Ctrl::Throw(_)) => {}
                Err(e) => return Err(e),
            }
            continue;
        }
        if !super::dom::progress(it)? {
            return it.type_err("await: the awaited promise can never settle");
        }
    }
}

fn this_promise(it: &mut Interp, a: &A) -> R<Gc> {
    match &a.this {
        Value::Obj(g) if matches!(it.heap.get(*g).kind, Kind::Promise(_)) => Ok(*g),
        _ => it.type_err("Promise method called on a non-promise"),
    }
}

fn combinator(it: &mut Interp, a: &A, kind: u8) -> R<Value> {
    // kind: 0 all, 1 allSettled, 2 race, 3 any
    let items = it.collect(&a.get(0))?;
    let result = new_promise(it);
    let n = items.len();
    let values = it.new_array(vec![Value::Undef; n]);
    let count = it.alloc(Obj::new(None, Kind::Boxed(num(n as f64))));
    if n == 0 {
        match kind {
            2 => {}
            3 => {
                let e = it.make_error(ErrKind::Error, "All promises were rejected");
                settle(it, result, PState::Rejected(e));
            }
            _ => settle(it, result, PState::Fulfilled(values)),
        }
        return Ok(Value::Obj(result));
    }
    fn element(it: &mut Interp, a: &A) -> R<Value> {
        let d = match &it.heap.get(a.callee).kind {
            Kind::Native(n) => n.data.clone(),
            _ => return Ok(Value::Undef),
        };
        // data: result, values, count, index, kind, which (0 fulfilled, 1 rejected)
        let (result, values, count) = (d[0].as_obj().unwrap(), d[1].clone(), d[2].as_obj().unwrap());
        let i = match d[3] {
            Value::Num(n) => n as usize,
            _ => 0,
        };
        let (kind, which) = (match d[4] { Value::Num(n) => n as u8, _ => 0 }, match d[5] { Value::Num(n) => n as u8, _ => 0 });
        let v = a.get(0);
        let stored = match (kind, which) {
            (1, w) => {
                let o = it.new_object();
                it.set_prop(o, "status", Value::str(if w == 0 { "fulfilled" } else { "rejected" }));
                it.set_prop(o, if w == 0 { "value" } else { "reason" }, v.clone());
                Value::Obj(o)
            }
            _ => v.clone(),
        };
        match (kind, which) {
            (0, 1) | (2, 1) => {
                settle(it, result, PState::Rejected(v));
                return Ok(Value::Undef);
            }
            (2, 0) | (3, 0) => {
                resolve_promise(it, result, v)?;
                return Ok(Value::Undef);
            }
            _ => {}
        }
        if let Value::Obj(vg) = &values {
            if let Kind::Array(arr) = &mut it.heap.get_mut(*vg).kind {
                if i < arr.len() {
                    arr[i] = stored;
                }
            }
        }
        let left = match &mut it.heap.get_mut(count).kind {
            Kind::Boxed(Value::Num(n)) => {
                *n -= 1.0;
                *n
            }
            _ => 1.0,
        };
        if left == 0.0 {
            if kind == 3 {
                let e = it.make_error(ErrKind::Error, "All promises were rejected");
                if let Value::Obj(eg) = &e {
                    it.hide(*eg, "errors", values);
                }
                settle(it, result, PState::Rejected(e));
            } else {
                settle(it, result, PState::Fulfilled(values));
            }
        }
        Ok(Value::Undef)
    }
    for (i, x) in items.into_iter().enumerate() {
        let pv = promise_resolved(it, x)?;
        let pg = pv.as_obj().unwrap();
        let base = vec![Value::Obj(result), values.clone(), Value::Obj(count), num(i as f64), num(kind as f64)];
        let mut d0 = base.clone();
        d0.push(num(0.0));
        let mut d1 = base;
        d1.push(num(1.0));
        let ok = it.new_native_data("", element, 1, d0);
        let bad = it.new_native_data("", element, 1, d1);
        promise_then(it, pg, Value::Obj(ok), Value::Obj(bad));
    }
    Ok(Value::Obj(result))
}

fn install_promise(it: &mut Interp) {
    let proto = it.p.promise.unwrap();
    let c = ctor(
        it,
        "Promise",
        |it, a| {
            if a.new_target.is_none() {
                return it.type_err("Promise constructor cannot be invoked without 'new'");
            }
            let exec = a.get(0);
            if !it.is_callable(&exec) {
                return it.type_err("Promise resolver is not a function");
            }
            let p = new_promise(it);
            let (res, rej) = resolving_functions(it, p);
            match it.call(&exec, Value::Undef, vec![res, rej.clone()]) {
                Ok(_) => {}
                Err(Ctrl::Throw(e)) => {
                    it.call(&rej, Value::Undef, vec![e])?;
                }
                Err(e) => return Err(e),
            }
            Ok(Value::Obj(p))
        },
        1,
        proto,
    );
    it.method(proto, "then", |it, a| {
        let p = this_promise(it, a)?;
        let ok = a.get(0);
        let bad = a.get(1);
        let ok = if it.is_callable(&ok) { ok } else { Value::Undef };
        let bad = if it.is_callable(&bad) { bad } else { Value::Undef };
        Ok(Value::Obj(promise_then(it, p, ok, bad)))
    }, 2);
    it.method(proto, "catch", |it, a| {
        let then = it.get(&a.this, "then")?;
        it.call(&then, a.this.clone(), vec![Value::Undef, a.get(0)])
    }, 1);
    it.method(proto, "finally", |it, a| {
        let p = this_promise(it, a)?;
        let f = a.get(0);
        if !it.is_callable(&f) {
            return Ok(Value::Obj(promise_then(it, p, Value::Undef, Value::Undef)));
        }
        let ok = it.new_native_data("", |it, a| {
            let f = match &it.heap.get(a.callee).kind {
                Kind::Native(n) => n.data[0].clone(),
                _ => Value::Undef,
            };
            it.call(&f, Value::Undef, Vec::new())?;
            Ok(a.get(0))
        }, 1, vec![f.clone()]);
        let bad = it.new_native_data("", |it, a| {
            let f = match &it.heap.get(a.callee).kind {
                Kind::Native(n) => n.data[0].clone(),
                _ => Value::Undef,
            };
            it.call(&f, Value::Undef, Vec::new())?;
            Err(Ctrl::Throw(a.get(0)))
        }, 1, vec![f]);
        Ok(Value::Obj(promise_then(it, p, Value::Obj(ok), Value::Obj(bad))))
    }, 1);
    it.method(c, "resolve", |it, a| promise_resolved(it, a.get(0)), 1);
    it.method(c, "reject", |it, a| Ok(promise_rejected(it, a.get(0))), 1);
    it.method(c, "all", |it, a| combinator(it, a, 0), 1);
    it.method(c, "allSettled", |it, a| combinator(it, a, 1), 1);
    it.method(c, "race", |it, a| combinator(it, a, 2), 1);
    it.method(c, "any", |it, a| combinator(it, a, 3), 1);
    it.method(c, "withResolvers", |it, _| {
        let p = new_promise(it);
        let (res, rej) = resolving_functions(it, p);
        let o = it.new_object();
        it.set_prop(o, "promise", Value::Obj(p));
        it.set_prop(o, "resolve", res);
        it.set_prop(o, "reject", rej);
        Ok(Value::Obj(o))
    }, 0);
}

// ---- globals ---------------------------------------------------------------------------------

const URI_RESERVED: &str = ";/?:@&=+$,#";
const URI_UNESCAPED: &str = "-_.!~*'()";

fn encode(it: &mut Interp, s: &str, keep: &str) -> R<Value> {
    let mut o = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || URI_UNESCAPED.contains(c) || keep.contains(c) {
            o.push(c);
        } else {
            let mut b = [0u8; 4];
            for x in c.encode_utf8(&mut b).bytes() {
                o.push_str(&format!("%{x:02X}"));
            }
        }
    }
    let _ = it;
    Ok(Value::Str(o.into()))
}

fn decode(it: &mut Interp, s: &str, keep: &str) -> R<Value> {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let h = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok());
            let Some(h) = h else { return Err(it.err(ErrKind::Error, "URIError: URI malformed")) };
            if h < 0x80 && keep.contains(h as char) {
                out.extend_from_slice(&b[i..i + 3]);
            } else {
                out.push(h);
            }
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    match String::from_utf8(out) {
        Ok(s) => Ok(Value::Str(s.into())),
        Err(_) => Err(it.err(ErrKind::Error, "URIError: URI malformed")),
    }
}

fn install_globals(it: &mut Interp) {
    let g = it.global;
    it.heap.get_mut(g).props.set("NaN".into(), Slot::Data(num(f64::NAN), 0));
    it.heap.get_mut(g).props.set("Infinity".into(), Slot::Data(num(f64::INFINITY), 0));
    it.heap.get_mut(g).props.set("undefined".into(), Slot::Data(Value::Undef, 0));
    it.hide(g, "globalThis", Value::Obj(g));
    it.method(g, "isNaN", |it, a| Ok(Value::Bool(it.to_num(&a.get(0))?.is_nan())), 1);
    it.method(g, "isFinite", |it, a| Ok(Value::Bool(it.to_num(&a.get(0))?.is_finite())), 1);
    it.method(g, "encodeURIComponent", |it, a| {
        let s = it.to_str(&a.get(0))?;
        encode(it, &s, "")
    }, 1);
    it.method(g, "encodeURI", |it, a| {
        let s = it.to_str(&a.get(0))?;
        encode(it, &s, URI_RESERVED)
    }, 1);
    it.method(g, "decodeURIComponent", |it, a| {
        let s = it.to_str(&a.get(0))?;
        decode(it, &s, "")
    }, 1);
    it.method(g, "decodeURI", |it, a| {
        let s = it.to_str(&a.get(0))?;
        decode(it, &s, URI_RESERVED)
    }, 1);
    it.method(g, "escape", |it, a| {
        let s = it.to_str(&a.get(0))?;
        let mut o = String::new();
        for c in s.chars() {
            if c.is_ascii_alphanumeric() || "@*_+-./".contains(c) {
                o.push(c);
            } else if (c as u32) < 256 {
                o.push_str(&format!("%{:02X}", c as u32));
            } else {
                let mut b = [0u16; 2];
                for u in c.encode_utf16(&mut b) {
                    o.push_str(&format!("%u{:04X}", u));
                }
            }
        }
        Ok(Value::Str(o.into()))
    }, 1);
    it.method(g, "unescape", |it, a| {
        let s: Vec<char> = it.to_str(&a.get(0))?.chars().collect();
        let mut units: Vec<u16> = Vec::new();
        let mut i = 0;
        while i < s.len() {
            if s[i] == '%' {
                if s.get(i + 1) == Some(&'u') && i + 6 <= s.len() {
                    if let Ok(v) = u16::from_str_radix(&s[i + 2..i + 6].iter().collect::<String>(), 16) {
                        units.push(v);
                        i += 6;
                        continue;
                    }
                }
                if i + 3 <= s.len() {
                    if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3].iter().collect::<String>(), 16) {
                        units.push(v as u16);
                        i += 3;
                        continue;
                    }
                }
            }
            let mut b = [0u16; 2];
            units.extend_from_slice(s[i].encode_utf16(&mut b));
            i += 1;
        }
        Ok(Value::Str(char::decode_utf16(units).map(|r| r.unwrap_or('\u{FFFD}')).collect::<String>().into()))
    }, 1);
    it.method(g, "queueMicrotask", |it, a| {
        let f = callback(it, a)?;
        it.microtasks.push_back((f, Vec::new()));
        Ok(Value::Undef)
    }, 1);
    it.method(g, "structuredClone", |it, a| {
        // A JSON round trip covers what pages use it for (plain data).
        let j = it.get(&Value::Obj(it.global), "JSON")?;
        let s = it.get(&j, "stringify")?;
        let p = it.get(&j, "parse")?;
        let t = it.call(&s, j.clone(), vec![a.get(0)])?;
        if matches!(t, Value::Undef) {
            return Ok(Value::Undef);
        }
        it.call(&p, j, vec![t])
    }, 1);

    // Symbol: symbols are strings here, in a reserved "@@" namespace that
    // for-in, Object.keys and JSON skip.
    let sym = it.new_native("Symbol", |it, a| {
        thread_local!(static N: std::cell::Cell<u64> = const { std::cell::Cell::new(0) });
        let n = N.with(|c| {
            c.set(c.get() + 1);
            c.get()
        });
        let d = match a.get(0) {
            Value::Undef => String::new(),
            v => it.to_str(&v)?.to_string(),
        };
        Ok(Value::Str(format!("@@sym:{n}:{d}").into()))
    }, 0);
    for (k, v) in [("iterator", "@@iterator"), ("asyncIterator", "@@asyncIterator"), ("hasInstance", "@@hasInstance"), ("toPrimitive", "@@toPrimitive"), ("toStringTag", "@@toStringTag"), ("species", "@@species")] {
        it.heap.get_mut(sym).props.set(k.into(), Slot::Data(Value::str(v), 0));
    }
    it.method(sym, "for", |it, a| {
        let k = it.to_str(&a.get(0))?;
        Ok(Value::Str(format!("@@for:{k}").into()))
    }, 1);
    it.hide(g, "Symbol", Value::Obj(sym));

    // Reflect.
    let r = it.new_object();
    it.hide(g, "Reflect", Value::Obj(r));
    it.method(r, "ownKeys", |it, a| {
        let Value::Obj(o) = a.get(0) else { return it.type_err("Reflect.ownKeys called on non-object") };
        let k = it.own_keys(o, false)?;
        Ok(it.new_array(k.into_iter().map(Value::Str).collect()))
    }, 1);
    it.method(r, "has", |it, a| {
        let Value::Obj(o) = a.get(0) else { return it.type_err("Reflect.has called on non-object") };
        let k = it.to_key(&a.get(1))?;
        Ok(Value::Bool(it.has_property(o, &k)?))
    }, 2);
    it.method(r, "get", |it, a| {
        let k = it.to_key(&a.get(1))?;
        it.get(&a.get(0), &k)
    }, 2);
    it.method(r, "set", |it, a| {
        let k = it.to_key(&a.get(1))?;
        it.put(&a.get(0), &k, a.get(2))?;
        Ok(Value::Bool(true))
    }, 3);
    it.method(r, "getPrototypeOf", |it, a| {
        let Value::Obj(o) = a.get(0) else { return it.type_err("Reflect.getPrototypeOf called on non-object") };
        Ok(it.heap.get(o).proto.map(Value::Obj).unwrap_or(Value::Null))
    }, 1);
    it.method(r, "apply", |it, a| {
        let args = array_like(it, &a.get(2))?;
        it.call(&a.get(0), a.get(1), args)
    }, 3);
    it.method(r, "construct", |it, a| {
        let args = array_like(it, &a.get(1))?;
        let nt = if a.args.len() > 2 { Some(a.get(2)) } else { None };
        it.construct(&a.get(0), args, nt)
    }, 2);
    it.method(r, "defineProperty", |it, a| {
        let Value::Obj(o) = a.get(0) else { return it.type_err("Reflect.defineProperty called on non-object") };
        let k = it.to_key(&a.get(1))?;
        let Value::Obj(d) = a.get(2) else { return Ok(Value::Bool(false)) };
        Ok(Value::Bool(define_property(it, o, &k, d).is_ok()))
    }, 3);
    it.method(r, "deleteProperty", |it, a| {
        let k = it.to_key(&a.get(1))?;
        Ok(Value::Bool(it.delete(&a.get(0), &k)?))
    }, 2);
    let _ = HashMap::<u8, u8>::new;
}
