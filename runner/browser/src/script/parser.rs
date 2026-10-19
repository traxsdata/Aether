//! script/parser.rs - tokens to a syntax tree, by recursive descent.
//!
//! The language is JavaScript as pages write it, minus the parts æther
//! leaves out on purpose or hasn't got yet: no `with`, no generators, no
//! modules (`import`/`export`), no `eval`. Those are syntax errors here (or,
//! for eval, a missing function), so a script that needs them stops cleanly
//! instead of half-running.

use std::rc::Rc;

use super::ast::*;
use super::lexer::{tokenize, Tok, Token};

#[derive(Debug, Clone)]
pub struct ParseError {
    pub msg: String,
    pub line: u32,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SyntaxError: {} (line {})", self.msg, self.line)
    }
}

/// How deeply expressions and statements may nest.
const MAX_DEPTH: usize = 300;

const RESERVED: &[&str] = &[
    "break", "case", "catch", "class", "const", "continue", "debugger", "default", "delete", "do", "else", "export", "extends", "finally", "for",
    "function", "if", "import", "in", "instanceof", "new", "return", "super", "switch", "this", "throw", "try", "typeof", "var", "void", "while",
    "with", "null", "true", "false", "enum", "yield",
];

struct FnCtx {
    vars: Vec<Name>,
    uses_args: bool,
    arrow: bool,
    is_async: bool,
}

struct Parser {
    t: Vec<Token>,
    i: usize,
    depth: usize,
    fns: Vec<FnCtx>,
}

type PR<T> = Result<T, ParseError>;

pub fn parse_program(src: &str) -> PR<Program> {
    let toks = tokenize(src).map_err(|e| ParseError { msg: e.msg, line: e.line })?;
    let mut p = Parser { t: toks, i: 0, depth: 0, fns: vec![FnCtx { vars: Vec::new(), uses_args: false, arrow: false, is_async: false }] };
    let mut body = Vec::new();
    while !p.at_eof() {
        body.push(p.stmt()?);
    }
    let ctx = p.fns.pop().unwrap();
    Ok(Program { body, vars: dedup(ctx.vars) })
}

fn dedup(mut v: Vec<Name>) -> Vec<Name> {
    let mut seen = std::collections::HashSet::new();
    v.retain(|n| seen.insert(n.clone()));
    v
}

fn pat_names(p: &Pat, out: &mut Vec<Name>) {
    match p {
        Pat::Id(n) => out.push(n.clone()),
        Pat::Obj(props, rest) => {
            for (_, p, _) in props {
                pat_names(p, out);
            }
            if let Some(r) = rest {
                pat_names(r, out);
            }
        }
        Pat::Arr(items, rest) => {
            for (p, _) in items.iter().flatten() {
                pat_names(p, out);
            }
            if let Some(r) = rest {
                pat_names(r, out);
            }
        }
        Pat::Expr(_) => {}
    }
}

fn needs_scope(s: &Stmt) -> bool {
    matches!(s, Stmt::Var(VarKind::Let | VarKind::Const, _) | Stmt::Class(_) | Stmt::Func(_))
}

impl Parser {
    // ---- token helpers ---------------------------------------------------

    fn tok(&self) -> &Tok {
        &self.t[self.i.min(self.t.len() - 1)].tok
    }

    fn peek_tok(&self, o: usize) -> &Tok {
        &self.t[(self.i + o).min(self.t.len() - 1)].tok
    }

    fn line(&self) -> u32 {
        self.t[self.i.min(self.t.len() - 1)].line
    }

    fn nl_before(&self) -> bool {
        self.t[self.i.min(self.t.len() - 1)].nl
    }

    fn nl_before_at(&self, o: usize) -> bool {
        self.t[(self.i + o).min(self.t.len() - 1)].nl
    }

    fn at_eof(&self) -> bool {
        matches!(self.tok(), Tok::Eof)
    }

    fn is_p(&self, p: &str) -> bool {
        matches!(self.tok(), Tok::P(x) if *x == p)
    }

    fn is_p_at(&self, o: usize, p: &str) -> bool {
        matches!(self.peek_tok(o), Tok::P(x) if *x == p)
    }

    fn is_kw(&self, k: &str) -> bool {
        matches!(self.tok(), Tok::Ident(x) if &**x == k)
    }

    fn is_kw_at(&self, o: usize, k: &str) -> bool {
        matches!(self.peek_tok(o), Tok::Ident(x) if &**x == k)
    }

    fn eat_p(&mut self, p: &str) -> bool {
        if self.is_p(p) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn eat_kw(&mut self, k: &str) -> bool {
        if self.is_kw(k) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn err<T>(&self, msg: impl Into<String>) -> PR<T> {
        Err(ParseError { msg: msg.into(), line: self.line() })
    }

    fn unexpected<T>(&self) -> PR<T> {
        let what = match self.tok() {
            Tok::Eof => "end of script".to_string(),
            Tok::P(p) => format!("'{p}'"),
            Tok::Ident(n) => format!("'{n}'"),
            Tok::Num(_) => "number".into(),
            Tok::Str(_) => "string".into(),
            Tok::Template { .. } => "template".into(),
            Tok::Regex(..) => "regular expression".into(),
            Tok::Private(n) => format!("'#{n}'"),
        };
        self.err(format!("unexpected {what}"))
    }

    fn expect_p(&mut self, p: &str) -> PR<()> {
        if self.eat_p(p) {
            Ok(())
        } else {
            self.unexpected()
        }
    }

    fn ident_name(&mut self) -> PR<Name> {
        match self.tok().clone() {
            Tok::Ident(n) => {
                self.i += 1;
                Ok(n)
            }
            _ => self.unexpected(),
        }
    }

    /// A binding identifier (not a reserved word).
    fn binding_ident(&mut self) -> PR<Name> {
        match self.tok().clone() {
            Tok::Ident(n) if !RESERVED.contains(&&*n) => {
                self.i += 1;
                Ok(n)
            }
            _ => self.unexpected(),
        }
    }

    fn semi(&mut self) -> PR<()> {
        if self.eat_p(";") || self.is_p("}") || self.at_eof() || self.nl_before() {
            return Ok(());
        }
        self.unexpected()
    }

    fn enter(&mut self) -> PR<()> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return self.err("nested too deeply");
        }
        Ok(())
    }

    fn fn_ctx(&mut self) -> &mut FnCtx {
        self.fns.last_mut().unwrap()
    }

    fn in_async(&self) -> bool {
        self.fns.last().is_some_and(|f| f.is_async)
    }

    // ---- statements --------------------------------------------------------

    fn stmt(&mut self) -> PR<Stmt> {
        self.enter()?;
        let r = self.stmt_inner();
        self.depth -= 1;
        r
    }

    fn stmt_inner(&mut self) -> PR<Stmt> {
        match self.tok().clone() {
            Tok::P("{") => Ok(Stmt::Block(self.block()?)),
            Tok::P(";") => {
                self.i += 1;
                Ok(Stmt::Empty)
            }
            Tok::Ident(k) => match &*k {
                "var" => self.var_stmt(VarKind::Var),
                "const" => self.var_stmt(VarKind::Const),
                "let" if matches!(self.peek_tok(1), Tok::Ident(_)) || self.is_p_at(1, "[") || self.is_p_at(1, "{") => self.var_stmt(VarKind::Let),
                "function" => {
                    self.i += 1;
                    let f = self.function(false, true)?;
                    Ok(Stmt::Func(f))
                }
                "async" if self.is_kw_at(1, "function") && !self.nl_before_at(1) => {
                    self.i += 2;
                    let f = self.function(true, true)?;
                    Ok(Stmt::Func(f))
                }
                "class" => {
                    self.i += 1;
                    let c = self.class(true)?;
                    Ok(Stmt::Class(c))
                }
                "if" => {
                    self.i += 1;
                    self.expect_p("(")?;
                    let c = self.expr(false)?;
                    self.expect_p(")")?;
                    let a = self.stmt()?;
                    let b = if self.eat_kw("else") { Some(Box::new(self.stmt()?)) } else { None };
                    Ok(Stmt::If(c, Box::new(a), b))
                }
                "for" => self.for_stmt(),
                "while" => {
                    self.i += 1;
                    self.expect_p("(")?;
                    let c = self.expr(false)?;
                    self.expect_p(")")?;
                    Ok(Stmt::While(c, Box::new(self.stmt()?)))
                }
                "do" => {
                    self.i += 1;
                    let body = self.stmt()?;
                    if !self.eat_kw("while") {
                        return self.unexpected();
                    }
                    self.expect_p("(")?;
                    let c = self.expr(false)?;
                    self.expect_p(")")?;
                    self.eat_p(";");
                    Ok(Stmt::DoWhile(Box::new(body), c))
                }
                "return" => {
                    self.i += 1;
                    if self.fns.len() <= 1 {
                        return self.err("return outside a function");
                    }
                    let v = if self.is_p(";") || self.is_p("}") || self.at_eof() || self.nl_before() { None } else { Some(self.expr(false)?) };
                    self.semi()?;
                    Ok(Stmt::Return(v))
                }
                "break" | "continue" => {
                    self.i += 1;
                    let label = match self.tok() {
                        Tok::Ident(n) if !self.nl_before() && !RESERVED.contains(&&**n) => {
                            let n = n.clone();
                            self.i += 1;
                            Some(n)
                        }
                        _ => None,
                    };
                    self.semi()?;
                    Ok(if &*k == "break" { Stmt::Break(label) } else { Stmt::Continue(label) })
                }
                "throw" => {
                    self.i += 1;
                    if self.nl_before() {
                        return self.err("line break after throw");
                    }
                    let v = self.expr(false)?;
                    self.semi()?;
                    Ok(Stmt::Throw(v))
                }
                "try" => self.try_stmt(),
                "switch" => self.switch_stmt(),
                "debugger" => {
                    self.i += 1;
                    self.semi()?;
                    Ok(Stmt::Empty)
                }
                "with" => self.err("'with' is not supported"),
                "import" if !self.is_p_at(1, "(") && !self.is_p_at(1, ".") => self.err("modules are not supported"),
                "export" => self.err("modules are not supported"),
                _ if self.is_p_at(1, ":") && !RESERVED.contains(&&*k) => {
                    self.i += 2;
                    let s = self.stmt()?;
                    Ok(Stmt::Labeled(k, Box::new(s)))
                }
                _ => self.expr_stmt(),
            },
            _ => self.expr_stmt(),
        }
    }

    fn expr_stmt(&mut self) -> PR<Stmt> {
        let e = self.expr(false)?;
        self.semi()?;
        Ok(Stmt::Expr(e))
    }

    fn block(&mut self) -> PR<Block> {
        self.expect_p("{")?;
        let mut body = Vec::new();
        while !self.is_p("}") {
            if self.at_eof() {
                return self.unexpected();
            }
            body.push(self.stmt()?);
        }
        self.i += 1;
        let scoped = body.iter().any(needs_scope);
        Ok(Block { body, scoped })
    }

    fn declarators(&mut self, kind: VarKind, no_in: bool) -> PR<Vec<(Pat, Option<Expr>)>> {
        let mut out = Vec::new();
        loop {
            let p = self.binding_pattern()?;
            let init = if self.eat_p("=") { Some(self.assign(no_in)?) } else { None };
            if kind == VarKind::Var {
                let mut names = Vec::new();
                pat_names(&p, &mut names);
                self.fn_ctx().vars.extend(names);
            }
            out.push((p, init));
            if !self.eat_p(",") {
                break;
            }
        }
        Ok(out)
    }

    fn var_stmt(&mut self, kind: VarKind) -> PR<Stmt> {
        self.i += 1;
        let d = self.declarators(kind, false)?;
        self.semi()?;
        Ok(Stmt::Var(kind, d))
    }

    fn for_stmt(&mut self) -> PR<Stmt> {
        self.i += 1;
        if self.is_kw("await") {
            return self.err("'for await' is not supported");
        }
        self.expect_p("(")?;
        let decl_kind = match self.tok() {
            Tok::Ident(k) if &**k == "var" => Some(VarKind::Var),
            Tok::Ident(k) if &**k == "const" => Some(VarKind::Const),
            Tok::Ident(k) if &**k == "let" && (matches!(self.peek_tok(1), Tok::Ident(_)) || self.is_p_at(1, "[") || self.is_p_at(1, "{")) => Some(VarKind::Let),
            _ => None,
        };
        let init: Option<Box<Stmt>>;
        if let Some(kind) = decl_kind {
            self.i += 1;
            let save = self.i;
            let p = self.binding_pattern()?;
            if self.is_kw("of") || self.is_kw("in") {
                let of = self.is_kw("of");
                self.i += 1;
                if kind == VarKind::Var {
                    let mut names = Vec::new();
                    pat_names(&p, &mut names);
                    self.fn_ctx().vars.extend(names);
                }
                let obj = if of { self.assign(false)? } else { self.expr(false)? };
                self.expect_p(")")?;
                let body = self.stmt()?;
                let head = ForHead::Decl(kind, p);
                return Ok(if of { Stmt::ForOf(head, obj, Box::new(body)) } else { Stmt::ForIn(head, obj, Box::new(body)) });
            }
            self.i = save;
            let d = self.declarators(kind, true)?;
            init = Some(Box::new(Stmt::Var(kind, d)));
        } else if self.is_p(";") {
            init = None;
        } else {
            let e = self.expr(true)?;
            if self.is_kw("of") || self.is_kw("in") {
                let of = self.is_kw("of");
                self.i += 1;
                let p = self.to_pat(e)?;
                let obj = if of { self.assign(false)? } else { self.expr(false)? };
                self.expect_p(")")?;
                let body = self.stmt()?;
                return Ok(if of { Stmt::ForOf(ForHead::Pat(p), obj, Box::new(body)) } else { Stmt::ForIn(ForHead::Pat(p), obj, Box::new(body)) });
            }
            init = Some(Box::new(Stmt::Expr(e)));
        }
        self.expect_p(";")?;
        let test = if self.is_p(";") { None } else { Some(self.expr(false)?) };
        self.expect_p(";")?;
        let update = if self.is_p(")") { None } else { Some(self.expr(false)?) };
        self.expect_p(")")?;
        let body = self.stmt()?;
        Ok(Stmt::For(init, test, update, Box::new(body)))
    }

    fn try_stmt(&mut self) -> PR<Stmt> {
        self.i += 1;
        let b = self.block()?;
        let mut catch = None;
        let mut fin = None;
        if self.eat_kw("catch") {
            let param = if self.eat_p("(") {
                let p = self.binding_pattern()?;
                self.expect_p(")")?;
                Some(p)
            } else {
                None
            };
            catch = Some((param, self.block()?));
        }
        if self.eat_kw("finally") {
            fin = Some(self.block()?);
        }
        if catch.is_none() && fin.is_none() {
            return self.err("try without catch or finally");
        }
        Ok(Stmt::Try(b, catch, fin))
    }

    fn switch_stmt(&mut self) -> PR<Stmt> {
        self.i += 1;
        self.expect_p("(")?;
        let d = self.expr(false)?;
        self.expect_p(")")?;
        self.expect_p("{")?;
        let mut cases = Vec::new();
        while !self.eat_p("}") {
            let test = if self.eat_kw("case") {
                Some(self.expr(false)?)
            } else if self.eat_kw("default") {
                None
            } else {
                return self.unexpected();
            };
            self.expect_p(":")?;
            let mut body = Vec::new();
            while !self.is_kw("case") && !self.is_kw("default") && !self.is_p("}") {
                if self.at_eof() {
                    return self.unexpected();
                }
                body.push(self.stmt()?);
            }
            cases.push((test, body));
        }
        Ok(Stmt::Switch(d, cases))
    }

    // ---- patterns ----------------------------------------------------------

    fn binding_pattern(&mut self) -> PR<Pat> {
        self.enter()?;
        let r = self.binding_pattern_inner();
        self.depth -= 1;
        r
    }

    fn binding_pattern_inner(&mut self) -> PR<Pat> {
        if self.eat_p("[") {
            let mut items = Vec::new();
            let mut rest = None;
            loop {
                if self.eat_p("]") {
                    break;
                }
                if self.eat_p(",") {
                    items.push(None);
                    continue;
                }
                if self.eat_p("...") {
                    rest = Some(Box::new(self.binding_pattern()?));
                    self.eat_p(",");
                    self.expect_p("]")?;
                    break;
                }
                let p = self.binding_pattern()?;
                let d = if self.eat_p("=") { Some(self.assign(false)?) } else { None };
                items.push(Some((p, d)));
                if !self.eat_p(",") {
                    self.expect_p("]")?;
                    break;
                }
            }
            return Ok(Pat::Arr(items, rest));
        }
        if self.eat_p("{") {
            let mut props = Vec::new();
            let mut rest = None;
            loop {
                if self.eat_p("}") {
                    break;
                }
                if self.eat_p("...") {
                    rest = Some(Box::new(Pat::Id(self.binding_ident()?)));
                    self.eat_p(",");
                    self.expect_p("}")?;
                    break;
                }
                let shorthand = matches!(self.tok(), Tok::Ident(_)) && !self.is_p_at(1, ":");
                let key = self.prop_name()?;
                let pat = if shorthand {
                    match &key {
                        PropName::Lit(n) if !RESERVED.contains(&&**n) => Pat::Id(n.clone()),
                        _ => return self.unexpected(),
                    }
                } else {
                    self.expect_p(":")?;
                    self.binding_pattern()?
                };
                let d = if self.eat_p("=") { Some(self.assign(false)?) } else { None };
                props.push((key, pat, d));
                if !self.eat_p(",") {
                    self.expect_p("}")?;
                    break;
                }
            }
            return Ok(Pat::Obj(props, rest));
        }
        Ok(Pat::Id(self.binding_ident()?))
    }

    /// An expression on the left of `=` (or in a for-in/of head) as a target.
    fn to_pat(&self, e: Expr) -> PR<Pat> {
        match e {
            Expr::Ident(n) => Ok(Pat::Id(n)),
            Expr::Member(..) | Expr::Index(..) => Ok(Pat::Expr(Box::new(e))),
            Expr::Array(items) => {
                let mut out = Vec::new();
                let mut rest = None;
                let n = items.len();
                for (k, it) in items.into_iter().enumerate() {
                    match it {
                        ArrEl::Hole => out.push(None),
                        ArrEl::Item(Expr::Assign(AssignOp::Set, p, d)) => out.push(Some((*p, Some(*d)))),
                        ArrEl::Item(e) => out.push(Some((self.to_pat(e)?, None))),
                        ArrEl::Spread(e) if k + 1 == n => rest = Some(Box::new(self.to_pat(e)?)),
                        ArrEl::Spread(_) => return self.err("rest element must be last"),
                    }
                }
                Ok(Pat::Arr(out, rest))
            }
            Expr::Object(props) => {
                let mut out = Vec::new();
                let mut rest = None;
                for p in props {
                    match p {
                        PropDef::Shorthand(n) => out.push((PropName::Lit(n.clone()), Pat::Id(n), None)),
                        PropDef::Value(k, Expr::Assign(AssignOp::Set, p, d)) => out.push((k, *p, Some(*d))),
                        PropDef::Value(k, e) => out.push((k, self.to_pat(e)?, None)),
                        PropDef::Spread(e) => rest = Some(Box::new(self.to_pat(e)?)),
                        _ => return self.err("invalid destructuring target"),
                    }
                }
                Ok(Pat::Obj(out, rest))
            }
            _ => self.err("invalid assignment target"),
        }
    }

    // ---- expressions ---------------------------------------------------------

    pub fn expr(&mut self, no_in: bool) -> PR<Expr> {
        let e = self.assign(no_in)?;
        if self.is_p(",") {
            let mut v = vec![e];
            while self.eat_p(",") {
                v.push(self.assign(no_in)?);
            }
            return Ok(Expr::Seq(v));
        }
        Ok(e)
    }

    /// Index of the `)` matching the `(` at token `from`.
    fn matching_paren(&self, from: usize) -> Option<usize> {
        let mut depth = 0i32;
        for j in from..self.t.len() {
            match &self.t[j].tok {
                Tok::P("(") | Tok::P("[") | Tok::P("{") => depth += 1,
                Tok::P(")") | Tok::P("]") | Tok::P("}") => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(j);
                    }
                }
                Tok::Eof => return None,
                _ => {}
            }
        }
        None
    }

    fn is_arrow_at(&self, o: usize) -> bool {
        match self.peek_tok(o) {
            Tok::Ident(n) if !RESERVED.contains(&&**n) => self.is_p_at(o + 1, "=>") && !self.nl_before_at(o + 1),
            Tok::P("(") => self.matching_paren(self.i + o).is_some_and(|j| matches!(self.t.get(j + 1).map(|t| &t.tok), Some(Tok::P("=>")))),
            _ => false,
        }
    }

    fn assign(&mut self, no_in: bool) -> PR<Expr> {
        self.enter()?;
        let r = self.assign_inner(no_in);
        self.depth -= 1;
        r
    }

    fn assign_inner(&mut self, no_in: bool) -> PR<Expr> {
        if self.is_arrow_at(0) {
            return self.arrow(false);
        }
        if self.is_kw("async") && !self.nl_before_at(1) && self.is_arrow_at(1) {
            self.i += 1;
            return self.arrow(true);
        }
        if self.is_kw("yield") {
            return self.err("generators are not supported");
        }
        let start = self.i;
        let lhs = self.conditional(no_in)?;
        let op = match self.tok() {
            Tok::P(p) => match *p {
                "=" => Some(AssignOp::Set),
                "+=" => Some(AssignOp::Op(BinOp::Add)),
                "-=" => Some(AssignOp::Op(BinOp::Sub)),
                "*=" => Some(AssignOp::Op(BinOp::Mul)),
                "/=" => Some(AssignOp::Op(BinOp::Div)),
                "%=" => Some(AssignOp::Op(BinOp::Mod)),
                "**=" => Some(AssignOp::Op(BinOp::Exp)),
                "<<=" => Some(AssignOp::Op(BinOp::Shl)),
                ">>=" => Some(AssignOp::Op(BinOp::Shr)),
                ">>>=" => Some(AssignOp::Op(BinOp::UShr)),
                "&=" => Some(AssignOp::Op(BinOp::BitAnd)),
                "|=" => Some(AssignOp::Op(BinOp::BitOr)),
                "^=" => Some(AssignOp::Op(BinOp::BitXor)),
                "&&=" => Some(AssignOp::Logic(LogicOp::And)),
                "||=" => Some(AssignOp::Logic(LogicOp::Or)),
                "??=" => Some(AssignOp::Logic(LogicOp::Nullish)),
                _ => None,
            },
            _ => None,
        };
        let Some(op) = op else { return Ok(lhs) };
        let target = if op == AssignOp::Set {
            // Parenthesised literals can't be patterns: `({a}) = x` is an error.
            if matches!(self.t[start].tok, Tok::P("(")) && matches!(lhs, Expr::Array(_) | Expr::Object(_)) {
                return self.err("invalid assignment target");
            }
            self.to_pat(lhs)?
        } else {
            match lhs {
                Expr::Ident(_) | Expr::Member(..) | Expr::Index(..) => self.to_pat(lhs)?,
                _ => return self.err("invalid assignment target"),
            }
        };
        self.i += 1;
        let rhs = self.assign(no_in)?;
        Ok(Expr::Assign(op, Box::new(target), Box::new(rhs)))
    }

    fn arrow(&mut self, is_async: bool) -> PR<Expr> {
        let line = self.line();
        self.fns.push(FnCtx { vars: Vec::new(), uses_args: false, arrow: true, is_async });
        let r = self.arrow_inner(is_async, line);
        self.fns.pop();
        r
    }

    fn arrow_inner(&mut self, is_async: bool, line: u32) -> PR<Expr> {
        let (params, rest) = if self.is_p("(") {
            self.params()?
        } else {
            (vec![(Pat::Id(self.binding_ident()?), None)], None)
        };
        self.expect_p("=>")?;
        let body = if self.is_p("{") {
            Body::Block(self.block()?.body)
        } else {
            Body::Expr(Box::new(self.assign(false)?))
        };
        let ctx = self.fns.last().unwrap();
        let simple = rest.is_none() && params.iter().all(|(p, d)| matches!(p, Pat::Id(_)) && d.is_none());
        let vars = dedup(ctx.vars.clone());
        let uses_args = ctx.uses_args;
        // An arrow's `arguments` is its enclosing function's.
        if uses_args {
            if let Some(outer) = self.fns.iter_mut().rev().nth(1) {
                outer.uses_args = true;
            }
        }
        Ok(Expr::Func(Rc::new(Func { name: None, params, rest, body, arrow: true, is_async, method: false, uses_args: false, vars, simple, line })))
    }

    fn params(&mut self) -> PR<(Vec<(Pat, Option<Expr>)>, Option<Pat>)> {
        self.expect_p("(")?;
        let mut params = Vec::new();
        let mut rest = None;
        loop {
            if self.eat_p(")") {
                break;
            }
            if self.eat_p("...") {
                rest = Some(self.binding_pattern()?);
                self.expect_p(")")?;
                break;
            }
            let p = self.binding_pattern()?;
            let d = if self.eat_p("=") { Some(self.assign(false)?) } else { None };
            params.push((p, d));
            if !self.eat_p(",") {
                self.expect_p(")")?;
                break;
            }
        }
        if params.len() > 255 {
            return self.err("too many parameters");
        }
        Ok((params, rest))
    }

    /// After `function` (and `async function`): name, parameters, body.
    fn function(&mut self, is_async: bool, decl: bool) -> PR<Rc<Func>> {
        if self.is_p("*") {
            return self.err("generators are not supported");
        }
        let name = if matches!(self.tok(), Tok::Ident(_)) && !self.is_p("(") {
            Some(self.binding_ident()?)
        } else if decl {
            return self.unexpected();
        } else {
            None
        };
        self.function_rest(name, is_async, false)
    }

    fn function_rest(&mut self, name: Option<Name>, is_async: bool, method: bool) -> PR<Rc<Func>> {
        let line = self.line();
        self.fns.push(FnCtx { vars: Vec::new(), uses_args: false, arrow: false, is_async });
        let r = (|| {
            let (params, rest) = self.params()?;
            let body = self.block()?.body;
            Ok((params, rest, body))
        })();
        let ctx = self.fns.pop().unwrap();
        let (params, rest, body) = r?;
        let simple = rest.is_none() && params.iter().all(|(p, d)| matches!(p, Pat::Id(_)) && d.is_none());
        Ok(Rc::new(Func { name, params, rest, body: Body::Block(body), arrow: false, is_async, method, uses_args: ctx.uses_args, vars: dedup(ctx.vars), simple, line }))
    }

    fn class(&mut self, decl: bool) -> PR<Rc<Class>> {
        let name = if matches!(self.tok(), Tok::Ident(n) if &**n != "extends") {
            Some(self.binding_ident()?)
        } else if decl {
            return self.unexpected();
        } else {
            None
        };
        let extends = if self.eat_kw("extends") { Some(self.lhs()?) } else { None };
        self.expect_p("{")?;
        let mut ctor = None;
        let mut members = Vec::new();
        loop {
            if self.eat_p("}") {
                break;
            }
            if self.eat_p(";") {
                continue;
            }
            if self.at_eof() {
                return self.unexpected();
            }
            let mut is_static = false;
            if self.is_kw("static") && !self.is_p_at(1, "(") && !self.is_p_at(1, "=") {
                self.i += 1;
                is_static = true;
                if self.is_p("{") {
                    return self.err("static blocks are not supported");
                }
            }
            let mut kind_word = None;
            if (self.is_kw("get") || self.is_kw("set") || self.is_kw("async")) && !self.is_p_at(1, "(") && !self.is_p_at(1, "=") && !self.is_p_at(1, ";") && !self.is_p_at(1, "}") && !self.nl_before_at(1) {
                kind_word = Some(self.ident_name()?);
            }
            if self.is_p("*") {
                return self.err("generators are not supported");
            }
            let key = self.class_key()?;
            if self.is_p("(") {
                let fname = match &key {
                    PropName::Lit(n) => Some(n.clone()),
                    _ => None,
                };
                let is_async = kind_word.as_deref() == Some("async");
                let f = self.function_rest(fname, is_async, true)?;
                let is_ctor = !is_static && kind_word.is_none() && matches!(&key, PropName::Lit(n) if &**n == "constructor");
                if is_ctor {
                    ctor = Some(f);
                    continue;
                }
                let kind = match kind_word.as_deref() {
                    Some("get") => MemberKind::Get(f),
                    Some("set") => MemberKind::Set(f),
                    _ => MemberKind::Method(f),
                };
                members.push(ClassMember { is_static, key, kind });
            } else {
                if kind_word.is_some() {
                    return self.unexpected();
                }
                // A field. Its initialiser runs like a method body (`this` is the instance).
                let init = if self.eat_p("=") {
                    self.fns.push(FnCtx { vars: Vec::new(), uses_args: false, arrow: false, is_async: false });
                    let e = self.assign(false);
                    self.fns.pop();
                    Some(e?)
                } else {
                    None
                };
                self.semi()?;
                members.push(ClassMember { is_static, key, kind: MemberKind::Field(init) });
            }
        }
        Ok(Rc::new(Class { name, extends, ctor, members }))
    }

    fn class_key(&mut self) -> PR<PropName> {
        if let Tok::Private(n) = self.tok().clone() {
            self.i += 1;
            return Ok(PropName::Lit(format!("#{n}").into()));
        }
        self.prop_name()
    }

    fn prop_name(&mut self) -> PR<PropName> {
        match self.tok().clone() {
            Tok::Ident(n) | Tok::Str(n) => {
                self.i += 1;
                Ok(PropName::Lit(n))
            }
            Tok::Num(v) => {
                self.i += 1;
                Ok(PropName::Lit(super::value::num_to_str(v).into()))
            }
            Tok::P("[") => {
                self.i += 1;
                let e = self.assign(false)?;
                self.expect_p("]")?;
                Ok(PropName::Computed(Box::new(e)))
            }
            _ => self.unexpected(),
        }
    }

    fn conditional(&mut self, no_in: bool) -> PR<Expr> {
        let c = self.binary(0, no_in)?;
        if self.eat_p("?") {
            let a = self.assign(false)?;
            self.expect_p(":")?;
            let b = self.assign(no_in)?;
            return Ok(Expr::Cond(Box::new(c), Box::new(a), Box::new(b)));
        }
        Ok(c)
    }

    fn binop(&self, no_in: bool) -> Option<(u8, Result<BinOp, LogicOp>)> {
        let p = match self.tok() {
            Tok::P(p) => *p,
            Tok::Ident(k) if &**k == "instanceof" => return Some((8, Ok(BinOp::InstanceOf))),
            Tok::Ident(k) if &**k == "in" && !no_in => return Some((8, Ok(BinOp::In))),
            _ => return None,
        };
        Some(match p {
            "??" => (1, Err(LogicOp::Nullish)),
            "||" => (2, Err(LogicOp::Or)),
            "&&" => (3, Err(LogicOp::And)),
            "|" => (4, Ok(BinOp::BitOr)),
            "^" => (5, Ok(BinOp::BitXor)),
            "&" => (6, Ok(BinOp::BitAnd)),
            "==" => (7, Ok(BinOp::Eq)),
            "!=" => (7, Ok(BinOp::Ne)),
            "===" => (7, Ok(BinOp::StrictEq)),
            "!==" => (7, Ok(BinOp::StrictNe)),
            "<" => (8, Ok(BinOp::Lt)),
            ">" => (8, Ok(BinOp::Gt)),
            "<=" => (8, Ok(BinOp::Le)),
            ">=" => (8, Ok(BinOp::Ge)),
            "<<" => (9, Ok(BinOp::Shl)),
            ">>" => (9, Ok(BinOp::Shr)),
            ">>>" => (9, Ok(BinOp::UShr)),
            "+" => (10, Ok(BinOp::Add)),
            "-" => (10, Ok(BinOp::Sub)),
            "*" => (11, Ok(BinOp::Mul)),
            "/" => (11, Ok(BinOp::Div)),
            "%" => (11, Ok(BinOp::Mod)),
            "**" => (12, Ok(BinOp::Exp)),
            _ => return None,
        })
    }

    fn binary(&mut self, min: u8, no_in: bool) -> PR<Expr> {
        let mut lhs = self.unary()?;
        while let Some((prec, op)) = self.binop(no_in) {
            if prec <= min && !(prec == 12 && min == 12) {
                break;
            }
            self.i += 1;
            // ** is right-associative.
            let rhs = if prec == 12 { self.binary(prec - 1, no_in)? } else { self.binary(prec, no_in)? };
            lhs = match op {
                Ok(b) => Expr::Bin(b, Box::new(lhs), Box::new(rhs)),
                Err(l) => Expr::Logic(l, Box::new(lhs), Box::new(rhs)),
            };
        }
        Ok(lhs)
    }

    fn unary(&mut self) -> PR<Expr> {
        self.enter()?;
        let r = self.unary_inner();
        self.depth -= 1;
        r
    }

    fn unary_inner(&mut self) -> PR<Expr> {
        let op = match self.tok() {
            Tok::P("!") => Some(UnOp::Not),
            Tok::P("-") => Some(UnOp::Neg),
            Tok::P("+") => Some(UnOp::Plus),
            Tok::P("~") => Some(UnOp::BitNot),
            Tok::Ident(k) if &**k == "typeof" => Some(UnOp::TypeOf),
            Tok::Ident(k) if &**k == "void" => Some(UnOp::Void),
            Tok::Ident(k) if &**k == "delete" => Some(UnOp::Delete),
            _ => None,
        };
        if let Some(op) = op {
            self.i += 1;
            let e = self.unary()?;
            return Ok(Expr::Unary(op, Box::new(e)));
        }
        if self.is_p("++") || self.is_p("--") {
            let inc = self.is_p("++");
            self.i += 1;
            let e = self.unary()?;
            if !matches!(e, Expr::Ident(_) | Expr::Member(..) | Expr::Index(..)) {
                return self.err("invalid increment target");
            }
            return Ok(Expr::Update(inc, true, Box::new(e)));
        }
        if self.is_kw("await") && self.in_async() {
            self.i += 1;
            let e = self.unary()?;
            return Ok(Expr::Await(Box::new(e)));
        }
        let e = self.lhs()?;
        if (self.is_p("++") || self.is_p("--")) && !self.nl_before() {
            if !matches!(e, Expr::Ident(_) | Expr::Member(..) | Expr::Index(..)) {
                return self.err("invalid increment target");
            }
            let inc = self.is_p("++");
            self.i += 1;
            return Ok(Expr::Update(inc, false, Box::new(e)));
        }
        Ok(e)
    }

    fn args(&mut self) -> PR<Vec<ArrEl>> {
        self.expect_p("(")?;
        let mut v = Vec::new();
        loop {
            if self.eat_p(")") {
                break;
            }
            if self.eat_p("...") {
                v.push(ArrEl::Spread(self.assign(false)?));
            } else {
                v.push(ArrEl::Item(self.assign(false)?));
            }
            if !self.eat_p(",") {
                self.expect_p(")")?;
                break;
            }
        }
        Ok(v)
    }

    fn member_name(&mut self) -> PR<Name> {
        match self.tok().clone() {
            Tok::Ident(n) => {
                self.i += 1;
                Ok(n)
            }
            Tok::Private(n) => {
                self.i += 1;
                Ok(format!("#{n}").into())
            }
            _ => self.unexpected(),
        }
    }

    fn template_parts(&mut self, toks: Vec<Vec<Token>>) -> PR<Vec<Expr>> {
        let mut out = Vec::new();
        for t in toks {
            let save_t = std::mem::replace(&mut self.t, t);
            let save_i = std::mem::replace(&mut self.i, 0);
            let r = self.expr(false).and_then(|e| if self.at_eof() { Ok(e) } else { self.unexpected() });
            self.t = save_t;
            self.i = save_i;
            out.push(r?);
        }
        Ok(out)
    }

    /// Calls, member accesses and `new` (a left-hand-side expression).
    fn lhs(&mut self) -> PR<Expr> {
        let mut e = if self.is_kw("new") {
            self.new_expr()?
        } else if self.is_kw("super") {
            self.i += 1;
            if self.is_p("(") {
                Expr::SuperCall(self.args()?)
            } else if self.eat_p(".") {
                Expr::SuperMember(self.member_name()?)
            } else if self.eat_p("[") {
                let k = self.expr(false)?;
                self.expect_p("]")?;
                Expr::SuperIndex(Box::new(k))
            } else {
                return self.unexpected();
            }
        } else {
            self.primary()?
        };
        let mut optional = false;
        let mut links = 0;
        loop {
            match self.tok().clone() {
                Tok::P(".") => {
                    self.i += 1;
                    e = Expr::Member(Box::new(e), self.member_name()?, false);
                }
                Tok::P("?.") => {
                    self.i += 1;
                    optional = true;
                    if self.is_p("(") {
                        e = Expr::Call(Box::new(e), self.args()?, true);
                    } else if self.eat_p("[") {
                        let k = self.expr(false)?;
                        self.expect_p("]")?;
                        e = Expr::Index(Box::new(e), Box::new(k), true);
                    } else {
                        e = Expr::Member(Box::new(e), self.member_name()?, true);
                    }
                }
                Tok::P("[") => {
                    self.i += 1;
                    let k = self.expr(false)?;
                    self.expect_p("]")?;
                    e = Expr::Index(Box::new(e), Box::new(k), false);
                }
                Tok::P("(") => {
                    let a = self.args()?;
                    e = Expr::Call(Box::new(e), a, false);
                }
                Tok::Template { cooked, raw, exprs } => {
                    if optional {
                        return self.err("tagged template in an optional chain");
                    }
                    self.i += 1;
                    let ex = self.template_parts(exprs)?;
                    e = Expr::TaggedTemplate(Box::new(e), Rc::new((cooked, raw)), ex);
                }
                _ => break,
            }
            // Each link nests the expression one level deeper.
            self.enter()?;
            links += 1;
        }
        self.depth -= links;
        Ok(if optional { Expr::OptChain(Box::new(e)) } else { e })
    }

    fn new_expr(&mut self) -> PR<Expr> {
        self.i += 1; // new
        if self.eat_p(".") {
            let n = self.ident_name()?;
            if &*n != "target" {
                return self.err("expected new.target");
            }
            return Ok(Expr::NewTarget);
        }
        self.enter()?;
        let mut callee = if self.is_kw("new") { self.new_expr()? } else { self.primary()? };
        loop {
            if self.eat_p(".") {
                callee = Expr::Member(Box::new(callee), self.member_name()?, false);
            } else if self.eat_p("[") {
                let k = self.expr(false)?;
                self.expect_p("]")?;
                callee = Expr::Index(Box::new(callee), Box::new(k), false);
            } else {
                break;
            }
        }
        self.depth -= 1;
        let args = if self.is_p("(") { self.args()? } else { Vec::new() };
        Ok(Expr::New(Box::new(callee), args))
    }

    fn primary(&mut self) -> PR<Expr> {
        let t = self.tok().clone();
        match t {
            Tok::Num(v) => {
                self.i += 1;
                Ok(Expr::Num(v))
            }
            Tok::Str(s) => {
                self.i += 1;
                Ok(Expr::Str(s))
            }
            Tok::Template { cooked, exprs, .. } => {
                self.i += 1;
                let ex = self.template_parts(exprs)?;
                Ok(Expr::Template(cooked, ex))
            }
            Tok::Regex(b, f) => {
                self.i += 1;
                Ok(Expr::Regex(b, f))
            }
            Tok::P("(") => {
                self.i += 1;
                let e = self.expr(false)?;
                self.expect_p(")")?;
                Ok(e)
            }
            Tok::P("[") => {
                self.i += 1;
                let mut items = Vec::new();
                loop {
                    if self.eat_p("]") {
                        break;
                    }
                    if self.eat_p(",") {
                        items.push(ArrEl::Hole);
                        continue;
                    }
                    if self.eat_p("...") {
                        items.push(ArrEl::Spread(self.assign(false)?));
                    } else {
                        items.push(ArrEl::Item(self.assign(false)?));
                    }
                    if !self.eat_p(",") {
                        self.expect_p("]")?;
                        break;
                    }
                }
                Ok(Expr::Array(items))
            }
            Tok::P("{") => self.object(),
            Tok::Ident(k) => match &*k {
                "this" => {
                    self.i += 1;
                    Ok(Expr::This)
                }
                "null" => {
                    self.i += 1;
                    Ok(Expr::Null)
                }
                "true" | "false" => {
                    self.i += 1;
                    Ok(Expr::Bool(&*k == "true"))
                }
                "function" => {
                    self.i += 1;
                    Ok(Expr::Func(self.function(false, false)?))
                }
                "async" if self.is_kw_at(1, "function") && !self.nl_before_at(1) => {
                    self.i += 2;
                    Ok(Expr::Func(self.function(true, false)?))
                }
                "class" => {
                    self.i += 1;
                    Ok(Expr::Class(self.class(false)?))
                }
                "import" => self.err("modules are not supported"),
                _ if RESERVED.contains(&&*k) => self.unexpected(),
                _ => {
                    self.i += 1;
                    if &*k == "arguments" {
                        if let Some(f) = self.fns.iter_mut().rev().find(|f| !f.arrow) {
                            f.uses_args = true;
                        }
                    }
                    Ok(Expr::Ident(k))
                }
            },
            _ => self.unexpected(),
        }
    }

    fn object(&mut self) -> PR<Expr> {
        self.i += 1; // {
        let mut props = Vec::new();
        loop {
            if self.eat_p("}") {
                break;
            }
            if self.eat_p("...") {
                props.push(PropDef::Spread(self.assign(false)?));
            } else {
                let mut word = None;
                if (self.is_kw("get") || self.is_kw("set") || self.is_kw("async"))
                    && !self.is_p_at(1, "(")
                    && !self.is_p_at(1, ":")
                    && !self.is_p_at(1, ",")
                    && !self.is_p_at(1, "}")
                    && !self.is_p_at(1, "=")
                {
                    word = Some(self.ident_name()?);
                }
                if self.is_p("*") {
                    return self.err("generators are not supported");
                }
                let shorthand_ok = matches!(self.tok(), Tok::Ident(n) if !RESERVED.contains(&&**n)) && word.is_none();
                let key = self.prop_name()?;
                if self.is_p("(") {
                    let fname = match &key {
                        PropName::Lit(n) => Some(n.clone()),
                        _ => None,
                    };
                    let is_async = word.as_deref() == Some("async");
                    let f = self.function_rest(fname, is_async, true)?;
                    props.push(match word.as_deref() {
                        Some("get") => PropDef::Get(key, f),
                        Some("set") => PropDef::Set(key, f),
                        _ => PropDef::Value(key, Expr::Func(f)),
                    });
                } else if word.is_some() {
                    return self.unexpected();
                } else if self.eat_p(":") {
                    props.push(PropDef::Value(key, self.assign(false)?));
                } else if shorthand_ok {
                    let PropName::Lit(n) = key else { return self.unexpected() };
                    if self.eat_p("=") {
                        // `{a = 1}`: only valid as a destructuring pattern.
                        let d = self.assign(false)?;
                        props.push(PropDef::Value(PropName::Lit(n.clone()), Expr::Assign(AssignOp::Set, Box::new(Pat::Id(n)), Box::new(d))));
                    } else {
                        props.push(PropDef::Shorthand(n));
                    }
                } else {
                    return self.unexpected();
                }
            }
            if !self.eat_p(",") {
                self.expect_p("}")?;
                break;
            }
        }
        Ok(Expr::Object(props))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(s: &str) {
        if let Err(e) = parse_program(s) {
            panic!("{s}: {e}");
        }
    }

    fn bad(s: &str) {
        assert!(parse_program(s).is_err(), "{s} should fail");
    }

    #[test]
    fn statements() {
        ok("var a = 1, b; let [c, , d = 2, ...e] = f; const {g, h: {i}, ...j} = k;");
        ok("for (let i = 0; i < 3; i++) {} for (const k in o) ; for (x of y) {} for (;;) break;");
        ok("a: for (;;) { continue a; } do x++; while (x < 3)");
        ok("try { a() } catch { } finally { b() } try {} catch ({message}) {}");
        ok("switch (x) { case 1: case 2: y(); break; default: z() }");
        ok("if (a) b(); else if (c) d(); else { e() }");
        ok("function f(a, b = 2, {c}, ...d) { return arguments.length }");
        ok("class A extends B { #p = 1; static s = 2; constructor() { super(); this.#p++ } get x() { return 1 } set x(v) {} static m() { super.m() } async n() {} }");
        ok("x = a\n++b\nc");
        ok("var f = async () => { await g(); }; async function h() { for (const x of await y) {} }");
    }

    #[test]
    fn expressions() {
        ok("a = b ? c : d ? e : f; a ??= b?.c?.[d]?.(e); x = y => y * 2; z = (a, {b}) => ({a, b});");
        ok("a = `x${b}y${`z${c}`}`; tag`a${b}`; new Foo; new Foo.Bar(1); new (f())(); new.target");
        ok("a = {get x() { return 1 }, set x(v) {}, [k]: 1, 'q': 2, 3: 4, m() {}, async n() {}, get: 1, set() {}, ...rest}");
        ok("[a, b] = [b, a]; ({a, b: {c}} = d); a = b ** c ** d; a = -b ** 2 > 0;");
        ok("x = /re/g.test(s) && y / 2 / z; if (/a/.test(b)) {}");
        ok("a = function () {}\n(b)");
        ok("let async = 1; async\n(x)");
    }

    #[test]
    fn errors() {
        crate::script::big_stack(errors_inner);
    }

    fn errors_inner() {
        bad("function* g() {}");
        bad("import x from 'y'");
        bad("export default 1");
        bad("with (a) {}");
        bad("a +");
        bad("1 = 2");
        bad("return 1");
        bad("({a}) = 1");
        bad("a b");
        bad(&"(".repeat(10_000));
        bad(&"[".repeat(10_000));
        bad(&"{".repeat(10_000));
    }
}
