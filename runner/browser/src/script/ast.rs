//! script/ast.rs - the syntax tree the parser builds and the interpreter walks.

use std::rc::Rc;

pub type Name = Rc<str>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Exp,
    Shl,
    Shr,
    UShr,
    BitAnd,
    BitOr,
    BitXor,
    Eq,
    Ne,
    StrictEq,
    StrictNe,
    Lt,
    Gt,
    Le,
    Ge,
    In,
    InstanceOf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogicOp {
    And,
    Or,
    Nullish,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Plus,
    Not,
    BitNot,
    TypeOf,
    Void,
    Delete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignOp {
    Set,
    Op(BinOp),
    Logic(LogicOp),
}

#[derive(Debug, Clone)]
pub enum ArrEl {
    Hole,
    Item(Expr),
    Spread(Expr),
}

#[derive(Debug, Clone)]
pub enum PropName {
    Lit(Name),
    Computed(Box<Expr>),
}

#[derive(Debug, Clone)]
pub enum PropDef {
    Value(PropName, Expr),
    Shorthand(Name),
    Spread(Expr),
    Get(PropName, Rc<Func>),
    Set(PropName, Rc<Func>),
}

/// A binding or assignment target.
#[derive(Debug, Clone)]
pub enum Pat {
    Id(Name),
    Obj(Vec<(PropName, Pat, Option<Expr>)>, Option<Box<Pat>>),
    Arr(Vec<Option<(Pat, Option<Expr>)>>, Option<Box<Pat>>),
    /// A member expression (assignment only).
    Expr(Box<Expr>),
}

#[derive(Debug, Clone)]
pub enum Expr {
    Num(f64),
    Str(Name),
    Template(Vec<Name>, Vec<Expr>),
    TaggedTemplate(Box<Expr>, Rc<(Vec<Name>, Vec<Name>)>, Vec<Expr>),
    Regex(Name, Name),
    Ident(Name),
    This,
    Null,
    Bool(bool),
    Array(Vec<ArrEl>),
    Object(Vec<PropDef>),
    Func(Rc<Func>),
    Class(Rc<Class>),
    Unary(UnOp, Box<Expr>),
    /// (increment, prefix, target)
    Update(bool, bool, Box<Expr>),
    Bin(BinOp, Box<Expr>, Box<Expr>),
    Logic(LogicOp, Box<Expr>, Box<Expr>),
    Assign(AssignOp, Box<Pat>, Box<Expr>),
    Cond(Box<Expr>, Box<Expr>, Box<Expr>),
    /// (callee, arguments, optional `?.()`)
    Call(Box<Expr>, Vec<ArrEl>, bool),
    New(Box<Expr>, Vec<ArrEl>),
    /// (object, name, optional `?.`)
    Member(Box<Expr>, Name, bool),
    Index(Box<Expr>, Box<Expr>, bool),
    /// The extent of an optional chain: a short-circuit stops here.
    OptChain(Box<Expr>),
    Seq(Vec<Expr>),
    SuperCall(Vec<ArrEl>),
    SuperMember(Name),
    SuperIndex(Box<Expr>),
    NewTarget,
    Await(Box<Expr>),
}

#[derive(Debug, Clone)]
pub enum Body {
    Block(Vec<Stmt>),
    Expr(Box<Expr>),
}

#[derive(Debug, Clone)]
pub struct Func {
    pub name: Option<Name>,
    pub params: Vec<(Pat, Option<Expr>)>,
    pub rest: Option<Pat>,
    pub body: Body,
    pub arrow: bool,
    pub is_async: bool,
    /// A method (object literal or class): has a home object, can't be `new`ed.
    pub method: bool,
    /// `arguments` appears in the body (not counting nested functions).
    pub uses_args: bool,
    /// `var` names declared in the body, hoisted to the function scope.
    pub vars: Vec<Name>,
    /// Parameters are plain names with no defaults (the fast path).
    pub simple: bool,
    pub line: u32,
}

#[derive(Debug, Clone)]
pub enum MemberKind {
    Method(Rc<Func>),
    Get(Rc<Func>),
    Set(Rc<Func>),
    Field(Option<Expr>),
}

#[derive(Debug, Clone)]
pub struct ClassMember {
    pub is_static: bool,
    pub key: PropName,
    pub kind: MemberKind,
}

#[derive(Debug, Clone)]
pub struct Class {
    pub name: Option<Name>,
    pub extends: Option<Expr>,
    pub ctor: Option<Rc<Func>>,
    pub members: Vec<ClassMember>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VarKind {
    Var,
    Let,
    Const,
}

#[derive(Debug, Clone)]
pub enum ForHead {
    Decl(VarKind, Pat),
    Pat(Pat),
}

#[derive(Debug, Clone)]
pub struct Block {
    pub body: Vec<Stmt>,
    /// Holds let, const, class or function declarations: needs its own scope.
    pub scoped: bool,
}

#[derive(Debug, Clone)]
pub enum Stmt {
    Var(VarKind, Vec<(Pat, Option<Expr>)>),
    Func(Rc<Func>),
    Class(Rc<Class>),
    Expr(Expr),
    If(Expr, Box<Stmt>, Option<Box<Stmt>>),
    Block(Block),
    For(Option<Box<Stmt>>, Option<Expr>, Option<Expr>, Box<Stmt>),
    ForIn(ForHead, Expr, Box<Stmt>),
    ForOf(ForHead, Expr, Box<Stmt>),
    While(Expr, Box<Stmt>),
    DoWhile(Box<Stmt>, Expr),
    Return(Option<Expr>),
    Break(Option<Name>),
    Continue(Option<Name>),
    Throw(Expr),
    Try(Block, Option<(Option<Pat>, Block)>, Option<Block>),
    Switch(Expr, Vec<(Option<Expr>, Vec<Stmt>)>),
    Labeled(Name, Box<Stmt>),
    Empty,
}

/// A parsed script.
#[derive(Debug, Clone)]
pub struct Program {
    pub body: Vec<Stmt>,
    /// Top-level `var` names (they become properties of the global object).
    pub vars: Vec<Name>,
}
