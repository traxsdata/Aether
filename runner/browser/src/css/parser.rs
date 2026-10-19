//! parser.rs - stylesheets: rules, declarations, selectors and the at-rules
//! that matter for reading pages (@media, @import, @supports, @layer).
//! @font-face is ignored on purpose: the browser never downloads fonts.

use super::tokenizer::{component_values, tokenize, Cv, Tok};

pub const MAX_RULES: usize = 20_000;
pub const MAX_COMPOUNDS: usize = 24;
const MAX_DECLS: usize = 400;
const MAX_RULE_NEST: usize = 8;

#[derive(Clone, Debug, PartialEq)]
pub struct Declaration {
    pub name: String,
    pub value: Vec<Cv>,
    pub important: bool,
}

#[derive(Clone, Debug)]
pub struct StyleRule {
    pub selectors: Vec<Selector>,
    pub decls: Vec<Declaration>,
}

#[derive(Clone, Debug, Default)]
pub struct Stylesheet {
    pub rules: Vec<StyleRule>,
    /// @import URLs (whose media matched), in order.
    pub imports: Vec<String>,
}

/// What @media queries are evaluated against.
#[derive(Clone, Copy, Debug)]
pub struct MediaEnv {
    pub width: f32,
    pub height: f32,
}

impl Default for MediaEnv {
    fn default() -> Self {
        MediaEnv { width: 1024.0, height: 768.0 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Combinator {
    Descendant,
    Child,
    Next,
    Subsequent,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AttrOp {
    Exists,
    Eq(String),
    Word(String),
    Dash(String),
    Prefix(String),
    Suffix(String),
    Contains(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct AttrSel {
    pub name: String,
    pub op: AttrOp,
    pub ci: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Pseudo {
    /// :link, :any-link (every <a href> is unvisited: the browser keeps no history for pages to probe)
    Link,
    /// Never matches: :visited, :hover, :focus, :active, :target ...
    Never,
    Root,
    Empty,
    FirstChild,
    LastChild,
    OnlyChild,
    FirstOfType,
    LastOfType,
    OnlyOfType,
    NthChild(i32, i32, bool),
    NthOfType(i32, i32, bool),
    Checked,
    Disabled,
    Enabled,
    Not(Vec<Selector>),
    Is(Vec<Selector>),
    Where(Vec<Selector>),
    Has,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Compound {
    pub tag: Option<String>,
    pub id: Option<String>,
    pub classes: Vec<String>,
    pub attrs: Vec<AttrSel>,
    pub pseudos: Vec<Pseudo>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PseudoElement {
    Before,
    After,
    Marker,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Selector {
    /// Left to right; combinators[i] joins compounds[i] and compounds[i + 1].
    pub compounds: Vec<Compound>,
    pub combinators: Vec<Combinator>,
    pub pseudo_element: Option<PseudoElement>,
    pub specificity: (u16, u16, u16),
}

impl Stylesheet {
    pub fn parse(src: &str, env: &MediaEnv) -> Stylesheet {
        let cvs = component_values(tokenize(src));
        let mut sheet = Stylesheet::default();
        parse_rules(&cvs, env, &mut sheet, 0, true);
        sheet
    }
}

/// Parses the declarations of a style="" attribute.
pub fn parse_inline(src: &str) -> Vec<Declaration> {
    parse_declarations(&component_values(tokenize(src)))
}

fn parse_rules(cvs: &[Cv], env: &MediaEnv, sheet: &mut Stylesheet, depth: usize, top: bool) {
    let mut i = 0;
    let mut imports_allowed = top;
    while i < cvs.len() {
        if sheet.rules.len() >= MAX_RULES {
            return;
        }
        match &cvs[i] {
            Cv::Tok(Tok::Ws) | Cv::Tok(Tok::Cdo) | Cv::Tok(Tok::Cdc) | Cv::Tok(Tok::Semicolon) => i += 1,
            Cv::Tok(Tok::AtKeyword(name)) => {
                let name = name.clone();
                // Prelude up to ';' or a {} block.
                let mut j = i + 1;
                while j < cvs.len() && !matches!(cvs[j], Cv::Tok(Tok::Semicolon) | Cv::Block('{', _)) {
                    j += 1;
                }
                let prelude = &cvs[i + 1..j];
                let block = match cvs.get(j) {
                    Some(Cv::Block('{', b)) => Some(b.as_slice()),
                    _ => None,
                };
                i = j + 1;
                match name.as_str() {
                    "import" if imports_allowed => {
                        if let Some(url) = import_url(prelude, env) {
                            sheet.imports.push(url);
                        }
                    }
                    "charset" | "namespace" => {}
                    "media" => {
                        imports_allowed = false;
                        if let Some(b) = block {
                            if depth < MAX_RULE_NEST && media_matches(prelude, env) {
                                parse_rules(b, env, sheet, depth + 1, false);
                            }
                        }
                    }
                    "supports" => {
                        imports_allowed = false;
                        if let Some(b) = block {
                            if depth < MAX_RULE_NEST && supports(prelude) {
                                parse_rules(b, env, sheet, depth + 1, false);
                            }
                        }
                    }
                    "layer" => {
                        if let Some(b) = block {
                            imports_allowed = false;
                            if depth < MAX_RULE_NEST {
                                parse_rules(b, env, sheet, depth + 1, false);
                            }
                        }
                    }
                    // @font-face (no downloaded fonts), @keyframes, @page, @container ...
                    _ => imports_allowed = imports_allowed && block.is_none(),
                }
            }
            _ => {
                imports_allowed = false;
                let mut j = i;
                while j < cvs.len() && !matches!(cvs[j], Cv::Block('{', _)) {
                    j += 1;
                }
                let Some(Cv::Block('{', body)) = cvs.get(j) else { return };
                if let Some(selectors) = parse_selector_list(&cvs[i..j]) {
                    let decls = parse_declarations(body);
                    if !decls.is_empty() {
                        sheet.rules.push(StyleRule { selectors, decls });
                    }
                }
                i = j + 1;
            }
        }
    }
}

fn import_url(prelude: &[Cv], env: &MediaEnv) -> Option<String> {
    let mut it = prelude.iter().filter(|c| !c.is_ws());
    let url = match it.next()? {
        Cv::Tok(Tok::Str(s)) | Cv::Tok(Tok::Url(s)) => s.clone(),
        Cv::Func(f, args) if f == "url" => match args.iter().find(|c| !c.is_ws())? {
            Cv::Tok(Tok::Str(s)) => s.clone(),
            _ => return None,
        },
        _ => return None,
    };
    let rest: Vec<Cv> = it.cloned().collect();
    // "layer(...)" and "supports(...)" before the media list are skipped.
    let media: Vec<Cv> = rest.into_iter().filter(|c| !matches!(c, Cv::Func(..)) && c.ident() != Some("layer")).collect();
    if media_matches(&media, env) {
        Some(url)
    } else {
        None
    }
}

/// Evaluates a media query list: screen and simple width/height/colour
/// features. Anything we can't evaluate makes that query false.
pub fn media_matches(prelude: &[Cv], env: &MediaEnv) -> bool {
    let parts = split_commas(prelude);
    if parts.iter().all(|p| p.iter().all(|c| c.is_ws())) {
        return true;
    }
    parts.iter().any(|q| media_query(q, env))
}

fn split_commas(cvs: &[Cv]) -> Vec<Vec<Cv>> {
    let mut out = vec![Vec::new()];
    for c in cvs {
        if matches!(c, Cv::Tok(Tok::Comma)) {
            out.push(Vec::new());
        } else {
            out.last_mut().unwrap().push(c.clone());
        }
    }
    out
}

fn media_query(q: &[Cv], env: &MediaEnv) -> bool {
    let items: Vec<&Cv> = q.iter().filter(|c| !c.is_ws()).collect();
    let mut negate = false;
    let mut ok = true;
    for (k, c) in items.iter().enumerate() {
        match c {
            Cv::Tok(Tok::Ident(w)) => match w.to_ascii_lowercase().as_str() {
                "not" if k == 0 => negate = true,
                "only" | "and" | "screen" | "all" => {}
                "or" => {}
                _ => ok = false, // print, speech, unknown types
            },
            Cv::Block('(', inner) => ok &= media_feature(inner, env),
            _ => ok = false,
        }
    }
    ok != negate
}

fn len_px(c: &Cv) -> Option<f32> {
    match c {
        Cv::Tok(Tok::Dimension(v, u)) => Some(match u.as_str() {
            "px" => *v,
            "em" | "rem" => v * 16.0,
            "pt" => v * 96.0 / 72.0,
            "cm" => v * 96.0 / 2.54,
            "mm" => v * 96.0 / 25.4,
            "in" => v * 96.0,
            "dppx" | "x" => *v,
            "dpi" => v / 96.0,
            _ => return None,
        }),
        Cv::Tok(Tok::Number(v)) => Some(*v),
        _ => None,
    }
}

fn media_feature(inner: &[Cv], env: &MediaEnv) -> bool {
    let items: Vec<&Cv> = inner.iter().filter(|c| !c.is_ws()).collect();
    // Range syntax: (width >= 600px), (400px < width <= 800px)
    if items.iter().any(|c| matches!(c, Cv::Tok(Tok::Delim('<' | '>' | '=')))) {
        return media_range(&items, env);
    }
    let Some(name) = items.first().and_then(|c| c.ident()).map(|s| s.to_ascii_lowercase()) else {
        // Nested (a) and (b) / not (a)
        if let Some(Cv::Block('(', b)) = items.first() {
            return media_feature(b, env);
        }
        return false;
    };
    let value = items.get(2).copied();
    let px = value.and_then(len_px);
    let id = value.and_then(|v| v.ident()).map(|s| s.to_ascii_lowercase());
    match (name.as_str(), px, id.as_deref()) {
        ("min-width", Some(v), _) => env.width >= v,
        ("max-width", Some(v), _) => env.width <= v,
        ("width", Some(v), _) => env.width == v,
        ("min-height", Some(v), _) => env.height >= v,
        ("max-height", Some(v), _) => env.height <= v,
        ("min-device-width", Some(v), _) => env.width >= v,
        ("max-device-width", Some(v), _) => env.width <= v,
        ("orientation", _, Some(o)) => (o == "landscape") == (env.width >= env.height),
        ("prefers-color-scheme", _, Some(s)) => s == "light",
        ("prefers-reduced-motion", _, Some(s)) => s == "reduce",
        ("prefers-contrast", _, Some(s)) => s == "no-preference",
        ("hover", _, Some(s)) | ("any-hover", _, Some(s)) => s == "hover",
        ("pointer", _, Some(s)) | ("any-pointer", _, Some(s)) => s == "fine",
        ("color", _, _) => true,
        ("min-resolution", Some(v), _) | ("min-device-pixel-ratio", Some(v), _) | ("-webkit-min-device-pixel-ratio", Some(v), _) => v <= 1.0,
        ("max-resolution", Some(v), _) | ("max-device-pixel-ratio", Some(v), _) | ("-webkit-max-device-pixel-ratio", Some(v), _) => v >= 1.0,
        ("scripting", _, Some(s)) => s == "none",
        ("display-mode", _, Some(s)) => s == "browser",
        ("forced-colors", _, Some(s)) => s == "none",
        ("update", _, Some(s)) => s == "fast",
        _ => false,
    }
}

fn media_range(items: &[&Cv], env: &MediaEnv) -> bool {
    // Collect operand / operator sequence.
    enum P {
        V(f32),
        Op(String),
    }
    let mut ps: Vec<P> = Vec::new();
    for c in items {
        match c {
            Cv::Tok(Tok::Delim(d)) if matches!(d, '<' | '>' | '=') => match ps.last_mut() {
                Some(P::Op(o)) => o.push(*d),
                _ => ps.push(P::Op(d.to_string())),
            },
            Cv::Tok(Tok::Ident(n)) => match n.to_ascii_lowercase().as_str() {
                "width" => ps.push(P::V(env.width)),
                "height" => ps.push(P::V(env.height)),
                _ => return false,
            },
            c => match len_px(c) {
                Some(v) => ps.push(P::V(v)),
                None => return false,
            },
        }
    }
    let mut k = 0;
    let mut ok = true;
    while k + 2 < ps.len() {
        match (&ps[k], &ps[k + 1], &ps[k + 2]) {
            (P::V(a), P::Op(o), P::V(b)) => {
                ok &= match o.as_str() {
                    "<" => a < b,
                    "<=" => a <= b,
                    ">" => a > b,
                    ">=" => a >= b,
                    "=" => a == b,
                    _ => false,
                }
            }
            _ => return false,
        }
        k += 2;
    }
    ok
}

/// @supports: true for declarations we implement, with not/and/or.
fn supports(prelude: &[Cv]) -> bool {
    let items: Vec<&Cv> = prelude.iter().filter(|c| !c.is_ws()).collect();
    let mut result: Option<bool> = None;
    let mut op = "and";
    let mut negate = false;
    for c in items {
        match c {
            Cv::Tok(Tok::Ident(w)) if w.eq_ignore_ascii_case("not") => negate = true,
            Cv::Tok(Tok::Ident(w)) if w.eq_ignore_ascii_case("and") => op = "and",
            Cv::Tok(Tok::Ident(w)) if w.eq_ignore_ascii_case("or") => op = "or",
            Cv::Block('(', inner) => {
                let v = supports_condition(inner) != negate;
                negate = false;
                result = Some(match (result, op) {
                    (None, _) => v,
                    (Some(r), "or") => r || v,
                    (Some(r), _) => r && v,
                });
            }
            _ => return false,
        }
    }
    result.unwrap_or(false)
}

fn supports_condition(inner: &[Cv]) -> bool {
    let items: Vec<&Cv> = inner.iter().filter(|c| !c.is_ws()).collect();
    if let Some(Cv::Block('(', _)) | Some(Cv::Tok(Tok::Ident(_))) = items.first() {
        if items.len() >= 2 && matches!(items[1], Cv::Tok(Tok::Colon)) {
            let name = items[0].ident().unwrap_or("").to_ascii_lowercase();
            let value: Vec<String> = items[2..].iter().filter_map(|c| c.ident().map(|s| s.to_ascii_lowercase())).collect();
            return match name.as_str() {
                "display" => value.iter().all(|v| ["block", "inline", "inline-block", "flex", "inline-flex", "none", "list-item", "table"].contains(&v.as_str())),
                n => super::style::SUPPORTED.contains(&n),
            };
        }
        return supports(inner);
    }
    false
}

/// Parses `name: value [!important]` declarations; rules nested inside
/// (CSS nesting) are skipped.
pub fn parse_declarations(cvs: &[Cv]) -> Vec<Declaration> {
    let mut out = Vec::new();
    for part in cvs.split(|c| matches!(c, Cv::Tok(Tok::Semicolon))) {
        if out.len() >= MAX_DECLS {
            break;
        }
        let mut items = part.iter().skip_while(|c| c.is_ws());
        let Some(Cv::Tok(Tok::Ident(name))) = items.next() else { continue };
        let rest: Vec<&Cv> = items.collect();
        let mut k = 0;
        while k < rest.len() && rest[k].is_ws() {
            k += 1;
        }
        if !matches!(rest.get(k), Some(Cv::Tok(Tok::Colon))) {
            continue;
        }
        let mut value: Vec<Cv> = rest[k + 1..].iter().map(|c| (*c).clone()).collect();
        if value.iter().any(|c| matches!(c, Cv::Block('{', _))) {
            continue;
        }
        while value.last().is_some_and(|c| c.is_ws()) {
            value.pop();
        }
        let mut important = false;
        let n = value.len();
        if n >= 2 {
            if let (Cv::Tok(Tok::Delim('!')), Some(id)) = (&value[n - 2], value[n - 1].ident()) {
                if id.eq_ignore_ascii_case("important") {
                    important = true;
                    value.truncate(n - 2);
                }
            }
        }
        while value.first().is_some_and(|c| c.is_ws()) {
            value.remove(0);
        }
        while value.last().is_some_and(|c| c.is_ws()) {
            value.pop();
        }
        let name = if name.starts_with("--") { name.clone() } else { name.to_ascii_lowercase() };
        out.push(Declaration { name, value, important });
    }
    out
}

/// A comma-separated selector list; None if any selector is invalid (the
/// whole rule is then dropped, as CSS requires).
pub fn parse_selector_list(cvs: &[Cv]) -> Option<Vec<Selector>> {
    let mut out = Vec::new();
    for part in split_commas(cvs) {
        out.push(parse_selector(&part)?);
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Like parse_selector_list, but drops invalid selectors (:is / :where).
fn parse_forgiving(cvs: &[Cv]) -> Vec<Selector> {
    split_commas(cvs).iter().filter_map(|p| parse_selector(p)).collect()
}

pub fn parse_selector(cvs: &[Cv]) -> Option<Selector> {
    let mut sel = Selector { compounds: vec![Compound::default()], combinators: Vec::new(), pseudo_element: None, specificity: (0, 0, 0) };
    let mut pending: Option<Combinator> = None;
    let mut empty = true; // the current compound has nothing yet
    let mut i = 0;
    let n = cvs.len();
    // Leading/trailing whitespace is insignificant.
    let mut end = n;
    while end > 0 && cvs[end - 1].is_ws() {
        end -= 1;
    }
    while i < end && cvs[i].is_ws() {
        i += 1;
    }
    if i >= end {
        return None;
    }
    while i < end {
        let c = &cvs[i];
        // Combinators.
        let comb = match c {
            Cv::Tok(Tok::Ws) => Some(Combinator::Descendant),
            Cv::Tok(Tok::Delim('>')) => Some(Combinator::Child),
            Cv::Tok(Tok::Delim('+')) => Some(Combinator::Next),
            Cv::Tok(Tok::Delim('~')) => Some(Combinator::Subsequent),
            _ => None,
        };
        if let Some(comb) = comb {
            if sel.pseudo_element.is_some() {
                return None;
            }
            if empty && sel.compounds.len() == 1 && comb != Combinator::Descendant {
                return None; // "> a" at the start
            }
            pending = match (pending, comb) {
                (None, c) => Some(c),
                (Some(Combinator::Descendant), c) => Some(c),
                (Some(p), Combinator::Descendant) => Some(p),
                _ => return None,
            };
            i += 1;
            continue;
        }
        if let Some(comb) = pending.take() {
            if empty {
                return None;
            }
            if sel.compounds.len() >= MAX_COMPOUNDS {
                return None;
            }
            sel.combinators.push(comb);
            sel.compounds.push(Compound::default());
            empty = true;
        }
        if sel.pseudo_element.is_some() && !matches!(c, Cv::Tok(Tok::Colon)) {
            return None;
        }
        let cur = sel.compounds.last_mut().unwrap();
        match c {
            Cv::Tok(Tok::Ident(name)) => {
                if !empty || cur.tag.is_some() {
                    return None;
                }
                cur.tag = Some(name.to_ascii_lowercase());
            }
            Cv::Tok(Tok::Delim('*')) => {
                if !empty {
                    return None;
                }
            }
            Cv::Tok(Tok::Delim('|')) => return None, // namespaces
            Cv::Tok(Tok::Hash(id)) => {
                if cur.id.is_some() && cur.id.as_deref() != Some(id) {
                    // Two different ids can never match; keep it impossible.
                    cur.pseudos.push(Pseudo::Never);
                }
                cur.id = Some(id.clone());
            }
            Cv::Tok(Tok::Delim('.')) => {
                i += 1;
                match cvs.get(i) {
                    Some(Cv::Tok(Tok::Ident(cl))) => cur.classes.push(cl.clone()),
                    _ => return None,
                }
            }
            Cv::Block('[', inner) => cur.attrs.push(parse_attr(inner)?),
            Cv::Tok(Tok::Colon) => {
                i += 1;
                let double = matches!(cvs.get(i), Some(Cv::Tok(Tok::Colon)));
                if double {
                    i += 1;
                }
                match cvs.get(i)? {
                    Cv::Tok(Tok::Ident(p)) => {
                        let p = p.to_ascii_lowercase();
                        match (double, p.as_str()) {
                            (_, "before") => sel.pseudo_element = Some(PseudoElement::Before),
                            (_, "after") => sel.pseudo_element = Some(PseudoElement::After),
                            (true, "marker") => sel.pseudo_element = Some(PseudoElement::Marker),
                            (true, _) | (false, "first-line") | (false, "first-letter") => return None,
                            (false, p) => cur.pseudos.push(simple_pseudo(p)?),
                        }
                    }
                    Cv::Func(f, args) if !double => cur.pseudos.push(func_pseudo(f, args)?),
                    _ => return None,
                }
            }
            _ => return None,
        }
        empty = false;
        i += 1;
    }
    if empty || pending.is_some() {
        return None;
    }
    sel.specificity = specificity(&sel);
    Some(sel)
}

fn simple_pseudo(p: &str) -> Option<Pseudo> {
    Some(match p {
        "link" | "any-link" | "-webkit-any-link" => Pseudo::Link,
        "visited" | "hover" | "focus" | "focus-visible" | "focus-within" | "active" | "target" | "target-within" | "playing"
        | "paused" | "fullscreen" | "modal" | "popover-open" | "placeholder-shown" | "autofill" | "-webkit-autofill" | "invalid"
        | "user-invalid" | "indeterminate" | "default" | "open" | "closed" | "scope" | "host" | "picture-in-picture"
        | "read-only" | "read-write" | "optional" | "required" | "valid" | "user-valid" | "in-range" | "out-of-range" => Pseudo::Never,
        "defined" => Pseudo::Is(vec![universal()]),
        "root" => Pseudo::Root,
        "empty" => Pseudo::Empty,
        "first-child" => Pseudo::FirstChild,
        "last-child" => Pseudo::LastChild,
        "only-child" => Pseudo::OnlyChild,
        "first-of-type" => Pseudo::FirstOfType,
        "last-of-type" => Pseudo::LastOfType,
        "only-of-type" => Pseudo::OnlyOfType,
        "checked" => Pseudo::Checked,
        "disabled" => Pseudo::Disabled,
        "enabled" => Pseudo::Enabled,
        _ => return None,
    })
}

fn universal() -> Selector {
    Selector { compounds: vec![Compound::default()], combinators: Vec::new(), pseudo_element: None, specificity: (0, 0, 0) }
}

fn func_pseudo(f: &str, args: &[Cv]) -> Option<Pseudo> {
    Some(match f {
        "not" => Pseudo::Not(parse_selector_list(args)?),
        "is" | "matches" | "-webkit-any" | "-moz-any" => Pseudo::Is(parse_forgiving(args)),
        "where" => Pseudo::Where(parse_forgiving(args)),
        "has" => Pseudo::Has,
        "nth-child" | "nth-last-child" | "nth-of-type" | "nth-last-of-type" => {
            let (a, b) = parse_nth(args)?;
            let last = f.contains("last");
            if f.contains("type") {
                Pseudo::NthOfType(a, b, last)
            } else {
                Pseudo::NthChild(a, b, last)
            }
        }
        "lang" | "dir" | "host" | "host-context" | "state" => Pseudo::Never,
        _ => return None,
    })
}

/// An+B: odd, even, 3, 2n+1, -n+3, n.
fn parse_nth(args: &[Cv]) -> Option<(i32, i32)> {
    let mut s = String::new();
    for c in args {
        match c {
            Cv::Tok(Tok::Ident(x)) => s += x,
            Cv::Tok(Tok::Number(v)) => {
                if *v >= 0.0 && !s.is_empty() && !s.ends_with(['+', '-']) {
                    s.push('+');
                }
                s += &format!("{}", *v as i64);
            }
            Cv::Tok(Tok::Dimension(v, u)) => s += &format!("{}{}", *v as i64, u),
            Cv::Tok(Tok::Delim(d)) => s.push(*d),
            Cv::Tok(Tok::Ws) => {}
            _ => return None,
        }
    }
    let s = s.to_ascii_lowercase();
    match s.as_str() {
        "odd" => return Some((2, 1)),
        "even" => return Some((2, 0)),
        _ => {}
    }
    if let Some(np) = s.find('n') {
        let a = match &s[..np] {
            "" | "+" => 1,
            "-" => -1,
            x => x.parse().ok()?,
        };
        let rest = s[np + 1..].replace("+-", "-");
        let b = if rest.is_empty() { 0 } else { rest.trim_start_matches('+').parse().ok()? };
        Some((a.clamp(-10000, 10000), b))
    } else {
        Some((0, s.trim_start_matches('+').parse().ok()?))
    }
}

fn parse_attr(inner: &[Cv]) -> Option<AttrSel> {
    let items: Vec<&Cv> = inner.iter().filter(|c| !c.is_ws()).collect();
    let name = items.first()?.ident()?.to_ascii_lowercase();
    if items.len() == 1 {
        return Some(AttrSel { name, op: AttrOp::Exists, ci: false });
    }
    let (op, k) = match (items.get(1)?, items.get(2)) {
        (Cv::Tok(Tok::Delim('=')), _) => ("=", 2),
        (Cv::Tok(Tok::Delim(d)), Some(Cv::Tok(Tok::Delim('=')))) if "~|^$*".contains(*d) => {
            (match d {
                '~' => "~=",
                '|' => "|=",
                '^' => "^=",
                '$' => "$=",
                _ => "*=",
            }, 3)
        }
        _ => return None,
    };
    let value = match items.get(k)? {
        Cv::Tok(Tok::Ident(v)) | Cv::Tok(Tok::Str(v)) => v.clone(),
        Cv::Tok(Tok::Number(v)) => format!("{v}"),
        _ => return None,
    };
    let ci = matches!(items.get(k + 1).and_then(|c| c.ident()), Some("i") | Some("I"));
    let op = match op {
        "=" => AttrOp::Eq(value),
        "~=" => AttrOp::Word(value),
        "|=" => AttrOp::Dash(value),
        "^=" => AttrOp::Prefix(value),
        "$=" => AttrOp::Suffix(value),
        _ => AttrOp::Contains(value),
    };
    Some(AttrSel { name, op, ci })
}

fn specificity(sel: &Selector) -> (u16, u16, u16) {
    let mut s = (0u16, 0u16, 0u16);
    for c in &sel.compounds {
        let cs = compound_specificity(c);
        s = (s.0.saturating_add(cs.0), s.1.saturating_add(cs.1), s.2.saturating_add(cs.2));
    }
    if sel.pseudo_element.is_some() {
        s.2 = s.2.saturating_add(1);
    }
    s
}

fn compound_specificity(c: &Compound) -> (u16, u16, u16) {
    let mut s = (c.id.is_some() as u16, (c.classes.len() + c.attrs.len()) as u16, c.tag.is_some() as u16);
    for p in &c.pseudos {
        let add = match p {
            Pseudo::Where(_) => (0, 0, 0),
            Pseudo::Not(list) | Pseudo::Is(list) => list.iter().map(|s| s.specificity).max().unwrap_or((0, 0, 0)),
            _ => (0, 1, 0),
        };
        s = (s.0 + add.0, s.1 + add.1, s.2 + add.2);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sel(s: &str) -> Option<Selector> {
        parse_selector(&component_values(tokenize(s)))
    }

    #[test]
    fn selectors() {
        let s = sel("div#main > ul.nav li a[href^='http']:first-child").unwrap();
        assert_eq!(s.compounds.len(), 4);
        assert_eq!(s.combinators, vec![Combinator::Child, Combinator::Descendant, Combinator::Descendant]);
        assert_eq!(s.specificity, (1, 3, 4));
        assert_eq!(sel("a , b"), None); // a list is not one selector
        assert!(sel("> a").is_none());
        assert!(sel("a >").is_none());
        assert!(sel("a:unknown-thing").is_none());
        assert!(sel("p::before").unwrap().pseudo_element == Some(PseudoElement::Before));
        assert!(sel(":not(.a, .b)").is_some());
        assert_eq!(sel(":where(.a) .b").unwrap().specificity, (0, 1, 0));
        assert_eq!(sel("li:nth-child(2n+1)").unwrap().compounds[0].pseudos, vec![Pseudo::NthChild(2, 1, false)]);
        assert_eq!(sel("li:nth-child(-n + 3)").unwrap().compounds[0].pseudos, vec![Pseudo::NthChild(-1, 3, false)]);
        assert_eq!(sel("li:nth-child(odd)").unwrap().compounds[0].pseudos, vec![Pseudo::NthChild(2, 1, false)]);
    }

    #[test]
    fn stylesheet_and_media() {
        let env = MediaEnv { width: 800.0, height: 600.0 };
        let s = Stylesheet::parse(
            "@import url(a.css) screen; @import 'b.css' print; @charset 'x';
             p { color: red; margin: 0 !important; }
             @media (max-width: 900px) { .narrow { display: none } }
             @media (min-width: 900px) { .wide { display: none } }
             @media print { .p { x: y } }
             @media screen and (width >= 600px) { .r { a: b } }
             @supports (display: flex) { .f { a: b } }
             @supports not (display: flex) { .nf { a: b } }
             @font-face { font-family: X; src: url(x.woff) }
             bad selector!! { a: b }
             h1, h2 { font-weight: bold; nested { x: y } }",
            &env,
        );
        assert_eq!(s.imports, vec!["a.css".to_string()]);
        let names: Vec<String> = s.rules.iter().map(|r| format!("{:?}", r.selectors[0].compounds[0])).collect();
        assert_eq!(s.rules.len(), 5, "{names:#?}");
        assert!(s.rules[0].decls[1].important);
        assert_eq!(s.rules[4].selectors.len(), 2);
        assert_eq!(s.rules[4].decls.len(), 1);
    }

    #[test]
    fn hostile_sheets() {
        let env = MediaEnv::default();
        let deep = "@media screen {".repeat(10_000) + "p{color:red}";
        let _ = Stylesheet::parse(&deep, &env);
        let many = "p{color:red}".repeat(50_000);
        assert!(Stylesheet::parse(&many, &env).rules.len() <= MAX_RULES);
        let long_sel = "a ".repeat(10_000) + "{x:y}";
        assert_eq!(Stylesheet::parse(&long_sel, &env).rules.len(), 0);
        let _ = Stylesheet::parse("p{color:", &env);
        let _ = Stylesheet::parse("}}}}{{{{", &env);
        let _ = Stylesheet::parse(":nth-child(99999999999999n+99999999999999){a:b}", &env);
        let _ = parse_inline("color: red; ;; : ; x");
    }
}
