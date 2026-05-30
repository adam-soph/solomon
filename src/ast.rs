//! Abstract syntax tree for HolyC.
//!
//! Every expression and statement carries a [`Span`] locating it in the source,
//! which later passes (type checking, codegen) use for diagnostics. To keep the
//! node shapes the focus of tests, `PartialEq` on AST nodes compares *structure
//! only* and ignores spans — so tests can build expected trees with
//! [`Span::dummy`].

use std::cell::RefCell;

use crate::token::{Keyword, Span};

/// A whole translation unit. HolyC is script-like: the top level is just a
/// sequence of statements, which may include function and class definitions.
#[derive(Clone, Debug, PartialEq)]
pub struct Program {
    pub items: Vec<Stmt>,
}

/// A HolyC type. Pointers and arrays wrap a base type.
#[derive(Clone, Debug, PartialEq)]
pub enum Type {
    U0, // void
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    I64,
    U64,
    F64,
    Bool,
    /// A class or union type referenced by name.
    Named(String),
    /// `T *`
    Ptr(Box<Type>),
    /// `T[n]` — the size expression is `None` for unsized arrays like `T[]`.
    Array(Box<Type>, Option<Box<Expr>>),
}

impl Type {
    /// Map a built-in type keyword to its [`Type`]. Returns `None` for keywords
    /// that are not type names.
    pub fn from_keyword(k: Keyword) -> Option<Type> {
        use Keyword as K;
        Some(match k {
            K::U0 => Type::U0,
            K::I8 => Type::I8,
            K::U8 => Type::U8,
            K::I16 => Type::I16,
            K::U16 => Type::U16,
            K::I32 => Type::I32,
            K::U32 => Type::U32,
            K::I64 => Type::I64,
            K::U64 => Type::U64,
            K::F64 => Type::F64,
            K::Bool => Type::Bool,
            _ => return None,
        })
    }
}

/// A single declared name with its (fully resolved) type and optional
/// initialiser. Used for variables and class fields.
#[derive(Clone, Debug)]
pub struct Declarator {
    pub name: String,
    pub ty: Type,
    pub init: Option<Expr>,
    pub span: Span,
}

/// A function parameter.
#[derive(Clone, Debug)]
pub struct Param {
    pub ty: Type,
    /// Prototypes may omit parameter names.
    pub name: Option<String>,
    /// HolyC supports default argument values.
    pub default: Option<Expr>,
    pub span: Span,
}

/// A function definition or prototype.
#[derive(Clone, Debug, PartialEq)]
pub struct FuncDef {
    pub ret: Type,
    pub name: String,
    pub params: Vec<Param>,
    /// Trailing `...` in the parameter list.
    pub varargs: bool,
    /// `None` for a prototype (`...;`), `Some(body)` for a definition.
    pub body: Option<Vec<Stmt>>,
}

/// A `class` or `union` definition.
#[derive(Clone, Debug, PartialEq)]
pub struct ClassDef {
    pub is_union: bool,
    pub name: String,
    /// `class Foo : Bar` inheritance.
    pub base: Option<String>,
    pub fields: Vec<Declarator>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Neg,    // -x
    Pos,    // +x
    Not,    // !x
    BitNot, // ~x
    Deref,  // *x
    AddrOf, // &x
    PreInc, // ++x
    PreDec, // --x
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PostOp {
    Inc, // x++
    Dec, // x--
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    And, // &&
    Or,  // ||
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssignOp {
    Assign, // =
    Add,    // +=
    Sub,    // -=
    Mul,    // *=
    Div,    // /=
    Mod,    // %=
    BitAnd, // &=
    BitOr,  // |=
    BitXor, // ^=
    Shl,    // <<=
    Shr,    // >>=
}

/// An expression node: its shape, its source span, and (after semantic
/// analysis) its inferred type.
///
/// The inferred type is stored in a `RefCell` so the type checker can annotate
/// the tree in place while passes keep their immutable `&Program` APIs. It is
/// `None` until semantic analysis has run over this node.
#[derive(Clone, Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
    ty: RefCell<Option<Type>>,
}

impl Expr {
    pub fn new(kind: ExprKind, span: Span) -> Self {
        Expr {
            kind,
            span,
            ty: RefCell::new(None),
        }
    }

    /// The inferred type of this expression, if semantic analysis has run.
    pub fn ty(&self) -> Option<Type> {
        self.ty.borrow().clone()
    }

    /// Record this expression's inferred type (called by semantic analysis).
    pub fn set_ty(&self, ty: Type) {
        *self.ty.borrow_mut() = Some(ty);
    }
}

/// The operand of `sizeof`: either a type or an expression whose static type is
/// used.
#[derive(Clone, Debug, PartialEq)]
pub enum SizeofArg {
    Type(Type),
    Expr(Box<Expr>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExprKind {
    Int(i64),
    Float(f64),
    Str(String),
    Char(i64),
    Ident(String),

    Unary {
        op: UnOp,
        expr: Box<Expr>,
    },
    Postfix {
        op: PostOp,
        expr: Box<Expr>,
    },
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    Assign {
        op: AssignOp,
        target: Box<Expr>,
        value: Box<Expr>,
    },
    Ternary {
        cond: Box<Expr>,
        then: Box<Expr>,
        else_: Box<Expr>,
    },
    Call {
        callee: Box<Expr>,
        args: Vec<Expr>,
    },
    Index {
        base: Box<Expr>,
        index: Box<Expr>,
    },
    Member {
        base: Box<Expr>,
        field: String,
        /// `true` for `->`, `false` for `.`.
        arrow: bool,
    },
    Cast {
        ty: Type,
        expr: Box<Expr>,
    },
    /// `sizeof(Type)` or `sizeof(expr)`. The size is computed at compile time
    /// from the type (for an expression, from its statically inferred type).
    Sizeof(SizeofArg),
    /// A comma-separated sequence. At statement level this is also how HolyC's
    /// implicit print works: `"x = %d\n", x` is a `Comma([Str, Ident])`.
    Comma(Vec<Expr>),
}

/// A statement node: its shape plus its source span.
#[derive(Clone, Debug)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

impl Stmt {
    pub fn new(kind: StmtKind, span: Span) -> Self {
        Stmt { kind, span }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum StmtKind {
    /// A lone `;`.
    Empty,
    Expr(Expr),
    Block(Vec<Stmt>),
    VarDecl {
        decls: Vec<Declarator>,
    },
    If {
        cond: Expr,
        then: Box<Stmt>,
        else_: Option<Box<Stmt>>,
    },
    While {
        cond: Expr,
        body: Box<Stmt>,
    },
    DoWhile {
        body: Box<Stmt>,
        cond: Expr,
    },
    For {
        init: Option<Box<Stmt>>,
        cond: Option<Expr>,
        step: Option<Expr>,
        body: Box<Stmt>,
    },
    Switch {
        cond: Expr,
        body: Box<Stmt>,
    },
    /// A `case` label. `hi` is set for HolyC range labels: `case lo ... hi:`.
    Case {
        lo: Expr,
        hi: Option<Expr>,
    },
    Default,
    Break,
    Continue,
    Return(Option<Expr>),
    Goto(String),
    Label(String),
    Func(FuncDef),
    Class(ClassDef),
    /// `#include "..."`.
    Include(String),
}

// ---- structural equality (spans ignored) ----
//
// These let tests assert on node shapes without having to predict exact byte
// offsets. Equality recurses through children, which use these same impls, so
// the whole tree is compared span-insensitively.

impl PartialEq for Expr {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind
    }
}

impl PartialEq for Stmt {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind
    }
}

impl PartialEq for Declarator {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name && self.ty == other.ty && self.init == other.init
    }
}

impl PartialEq for Param {
    fn eq(&self, other: &Self) -> bool {
        self.ty == other.ty && self.name == other.name && self.default == other.default
    }
}
