//! Abstract syntax tree for HolyC.
//!
//! Every expression and statement carries a [`Span`] locating it in the source.
//! Later passes (type checking, codegen) use spans for diagnostics. Each `Expr`
//! also carries an inferred type (`ty`, a `RefCell<Option<Type>>`) that semantic
//! analysis fills in via interior mutability.
//!
//! `PartialEq` on AST nodes compares structure only; it ignores both spans and
//! `ty`. This keeps node shapes the focus of tests and lets them build expected
//! trees with [`Span::dummy`].

use std::cell::RefCell;
use std::collections::HashMap;

use crate::token::{FileInfo, Keyword, Span};

/// A whole translation unit. HolyC is script-like: the top level is just a
/// sequence of statements, which may include function and class definitions.
#[derive(Clone, Debug)]
pub struct Program {
    pub items: Vec<Stmt>,
    /// The source files seen during parsing, indexed by `Span::file`. Each entry
    /// carries the file's directory, which sema uses for `_`-directory privacy
    /// checks. This is provenance metadata, so `PartialEq` ignores it, just as it
    /// ignores spans.
    pub files: Vec<FileInfo>,
    /// The generic-template registry captured during parsing. It is the input to
    /// the [`mono`](crate::mono) pass, which instantiates every deferred generic
    /// use and leaves this empty, so a post-`mono` `Program` is fully concrete.
    /// Like `files`, it is ignored by `PartialEq`.
    pub generics: GenericTemplates,
}

impl PartialEq for Program {
    fn eq(&self, other: &Self) -> bool {
        self.items == other.items
    }
}

/// The generic `class` and function templates captured during parsing. They are
/// carried on the [`Program`] for the [`mono`](crate::mono) pass to instantiate.
/// The parser leaves every generic *use* deferred, as a `Type::Generic`,
/// `Type::Tuple`, `ExprKind::GenericCall`, or `StmtKind::ShortDecl`. `mono` then
/// consumes these templates and resolves the deferred uses.
#[derive(Clone, Debug, Default)]
pub struct GenericTemplates {
    /// Generic `class`/`union` templates, by name.
    pub classes: HashMap<String, GenericClass>,
    /// Generic function templates, by name.
    pub fns: HashMap<String, GenericFn>,
}

/// A constraint a type parameter must satisfy at instantiation. Currently only
/// `comparable` (a type orderable by `<`/`>`, i.e. a scalar or pointer); the enum is
/// a seam for more constraints later.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Constraint {
    /// `comparable T` — `T` must be a type sema accepts as a relational operand.
    Comparable,
}

/// A generic parameter declared in a `<...>` list. Either a **type** parameter
/// (`type T` / bare `T`, optionally `comparable`-constrained) or a **value**
/// (non-type) parameter (`int N`, an integer constant).
#[derive(Clone, Debug, PartialEq)]
pub enum GenericParam {
    /// A type parameter, with an optional constraint.
    Type(String, Option<Constraint>),
    /// An integer value parameter (`int N`).
    Value(String),
}

impl GenericParam {
    /// The parameter's name.
    pub fn name(&self) -> &str {
        match self {
            GenericParam::Type(n, _) | GenericParam::Value(n) => n,
        }
    }
}

/// A generic argument at a use site: either a type (`Vec<I64>`) or a value
/// (`FixedArr<I64, 8>`). The value arm carries an `Expr` because it may reference an
/// enclosing template's value parameter; it only becomes a literal `Int` once that
/// template is instantiated and the expression is const-evaluated.
#[derive(Clone, Debug, PartialEq)]
pub enum GenericArg {
    Type(Type),
    Value(Box<Expr>),
}

/// What a compile-time type switch ([`StmtKind::TypeSwitch`]) dispatches on: either a
/// type directly (`switch type (T)`) or the static type of an expression
/// (`switch type (x)`).
#[derive(Clone, Debug, PartialEq)]
pub enum TypeSwitchOn {
    Ty(Type),
    Val(Box<Expr>),
}

/// A captured generic-function template. Holds its parameters and the function
/// parsed once into a `FuncDef` with those parameters left symbolic. The
/// [`mono`](crate::mono) pass substitutes the parameters in this AST for each
/// instantiation, with no token re-parse.
#[derive(Clone, Debug)]
pub struct GenericFn {
    /// The generic parameters, in order (type and/or value).
    pub params: Vec<GenericParam>,
    /// The template `FuncDef`, with parameters left symbolic. A `T` type is a
    /// `Type::Param`, a nested `Vec<T>` a deferred `Type::Generic`, a value param `N`
    /// an ordinary `Expr::Ident` (in array dims / expressions), a generic call in the
    /// body an `ExprKind::GenericCall`, and a `:=` a `StmtKind::ShortDecl`.
    pub def: FuncDef,
}

/// A captured generic `class`/`union` template.
#[derive(Clone, Debug)]
pub struct GenericClass {
    pub is_union: bool,
    /// The generic parameters, e.g. `[type T]` or `[type K, type V]` or
    /// `[type T, int N]`.
    pub params: Vec<GenericParam>,
    pub base: Option<String>,
    /// The fields, parsed once with the template's parameters left symbolic. A `T`
    /// field type is `Type::Param("T")`, a nested `Vec<T>` is `Type::Generic(...)`,
    /// and a value-param array dim `T data[N]` keeps `N` as an `Expr::Ident`. The
    /// [`mono`](crate::mono) pass substitutes the parameters in this AST, with no
    /// re-parse.
    pub fields: Vec<Declarator>,
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
    /// `T[n]`. The size expression is `None` for unsized arrays like `T[]`.
    Array(Box<Type>, Option<Box<Expr>>),
    /// A function pointer, `ret (*)(params...)`. It is an 8-byte scalar like any
    /// pointer. The signature drives call type-checking and argument classing.
    FuncPtr {
        ret: Box<Type>,
        params: Vec<Type>,
    },
    /// A generic type parameter (`T`) inside a template body. It is replaced with
    /// a concrete type when the template is instantiated. It appears only in a
    /// generic template's AST: the monomorphization pass resolves it away, so sema,
    /// layout, and the backends never see it.
    Param(String),
    /// An un-instantiated generic application (`Vec<T>`, `FixedArr<I64, 8>`) inside a
    /// template body or use site. Its arguments are [`GenericArg`]s (types and/or
    /// values). It resolves to a concrete `Named` once the arguments are bound at
    /// instantiation. Like [`Type::Param`], it never reaches sema, layout, or the
    /// backends.
    Generic(String, Vec<GenericArg>),
    /// A deferred tuple type `(T0, …, Tn)` inside a template body. Its elements may
    /// be parametric, so it isn't interned into a `$Tup` class until the parameters
    /// are bound at instantiation; substitution interns the concrete tuple. Outside
    /// a template a tuple type is interned immediately to a `Named`, so this variant
    /// never reaches sema, layout, or the backends.
    Tuple(Vec<Type>),
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

/// A single declared name with its fully resolved type and optional initialiser.
/// Used for variables and class fields.
#[derive(Clone, Debug)]
pub struct Declarator {
    pub name: String,
    pub ty: Type,
    pub init: Option<Expr>,
    pub span: Span,
    /// `public` modifier: a top-level global declared `public` is visible from any
    /// file; otherwise it is private to its defining file. Meaningless (and ignored)
    /// for locals and class fields. Like `span`, it is excluded from `PartialEq`.
    pub is_public: bool,
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
    /// `public` modifier: a `public` function is callable from any file; otherwise it
    /// is private to its defining file (visibility is file-scoped). Monomorphized
    /// generic instances are always public.
    pub is_public: bool,
}

/// A `class` or `union` definition.
#[derive(Clone, Debug, PartialEq)]
pub struct ClassDef {
    pub is_union: bool,
    pub name: String,
    /// `class Foo : Bar` inheritance.
    pub base: Option<String>,
    pub fields: Vec<Declarator>,
    /// `public` modifier: a `public` class/union is usable from any file; otherwise it
    /// is private to its defining file. Compiler-synthesized aggregates (tuples,
    /// anonymous aggregates) and monomorphized generic instances are always public.
    pub is_public: bool,
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

/// An expression node. Holds its shape, its source span, and (after semantic
/// analysis) its inferred type.
///
/// The inferred type lives in a `RefCell` so the type checker can annotate the
/// tree in place while passes keep their immutable `&Program` APIs. It is `None`
/// until semantic analysis has run over this node.
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

    /// Record this expression's inferred type. Called by semantic analysis.
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
    /// A deferred generic call inside a generic-function template body, such as
    /// `VecReserve<T>(v, …)` or an inferred `VecPush(&v, x)` (with `type_args`
    /// empty). The type parameters aren't bound until the enclosing template is
    /// instantiated, so the call can't be resolved at parse time. The
    /// monomorphization substitution resolves it: it substitutes `type_args` (or
    /// re-infers them), then rewrites the node to a concrete `Call`. Never reaches
    /// sema, layout, or the backends.
    GenericCall {
        name: String,
        type_args: Vec<GenericArg>,
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
    /// `sizeof(Type)` or `sizeof(expr)`. The size is computed at compile time from
    /// the type. For an expression, it uses the expression's statically inferred type.
    Sizeof(SizeofArg),
    /// `offset(ClassName.field[.field...])`. The byte offset of a member within a
    /// class, possibly nested, computed at compile time. This is HolyC's equivalent
    /// of C's `offsetof`.
    Offset {
        class: String,
        path: Vec<String>,
    },
    /// A brace-enclosed aggregate initializer, e.g. `{1, 2, 3}` or `{{1, 2}, {3, 4}}`.
    /// Valid only as a variable or field initializer. It is type-checked against the
    /// declared aggregate type, whether array or class.
    InitList(Vec<Expr>),
    /// A brace-enclosed designated initializer, e.g. `{.x = 1, .y = 2}`. Each item
    /// names a field of the target class. Fields may appear in any order, and
    /// omitted ones take their default of zero. Valid only for class types.
    DesignatedInit(Vec<(String, Expr)>),
    /// A comma-separated sequence. At statement level this is also how HolyC's
    /// implicit print works: `"x = %d\n", x` is a `Comma([Str, Ident])`.
    Comma(Vec<Expr>),
}

/// A statement node. Holds its shape plus its source span.
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
    /// A deferred `:=` inside a generic-function template body, such as `a, b := rhs;`
    /// or `n := rhs;`. Its element or variable types can't be inferred until the
    /// template's parameters are bound, e.g. `_, ok := HmapGet<K, V>(m, key)`. The
    /// monomorphization substitution desugars it to a `VarDecl` once `rhs` is
    /// concrete. A `None` name is a `_` discard. Never reaches sema, layout, or the
    /// backends.
    ShortDecl {
        names: Vec<Option<String>>,
        rhs: Expr,
    },
    /// A deferred compile-time type switch (`switch type (T) { case I64: … }`) inside
    /// a generic template. The [`mono`](crate::mono) pass resolves the scrutinee to a
    /// concrete type, keeps only the matching arm (or `default`), and replaces the
    /// node with that arm's statements — discarding the others before sema. Never
    /// reaches sema, layout, or the backends.
    TypeSwitch {
        on: TypeSwitchOn,
        /// `(case type, body)` arms, in source order. First match wins.
        arms: Vec<(Type, Vec<Stmt>)>,
        default: Option<Vec<Stmt>>,
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
    /// HolyC's `start:` switch sub-label. Marks the start of a switch prologue:
    /// statements that run on entry, before dispatch.
    SwitchStart,
    /// HolyC's `end:` switch sub-label. Marks the start of a switch epilogue:
    /// statements reached by fall-through. A `break` skips them.
    SwitchEnd,
    Break,
    Continue,
    Return(Option<Expr>),
    Goto(String),
    Label(String),
    Func(FuncDef),
    Class(ClassDef),
    /// `#include "..."`.
    Include(String),
    /// `try { body } catch { handler }`. If the `body` (or anything it calls) throws,
    /// control transfers to `handler`, where the thrown value is `Fs->except_ch`. The
    /// `catch` block takes no parameter (HolyC form).
    Try {
        body: Vec<Stmt>,
        handler: Vec<Stmt>,
    },
    /// `throw expr;` raises an exception carrying `expr`'s value (coerced to `I64`),
    /// unwinding to the nearest enclosing `try`. A bare `throw;` (`None`) re-raises the
    /// current `Fs->except_ch`.
    Throw(Option<Expr>),
}

// ---- structural equality (spans ignored) ----
//
// These let tests assert on node shapes without predicting exact byte offsets.
// Equality recurses through children, which use these same impls, so the whole
// tree is compared span-insensitively.

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

/// Whether the program contains a call to any function named in `names`. The
/// native backends use this to decide whether to emit command-line-argument
/// capture in the program entry. A program that never references `ArgC` or `ArgV`
/// is then byte-for-byte unaffected by the feature.
pub fn program_calls_any(program: &Program, names: &[&str]) -> bool {
    program.items.iter().any(|s| stmt_calls(s, names))
}

fn stmt_calls(s: &Stmt, names: &[&str]) -> bool {
    match &s.kind {
        StmtKind::Empty
        | StmtKind::Default
        | StmtKind::SwitchStart
        | StmtKind::SwitchEnd
        | StmtKind::Break
        | StmtKind::Continue
        | StmtKind::Goto(_)
        | StmtKind::Label(_)
        | StmtKind::Class(_)
        | StmtKind::Include(_) => false,
        StmtKind::Expr(e) => expr_calls(e, names),
        StmtKind::ShortDecl { rhs, .. } => expr_calls(rhs, names),
        StmtKind::Block(ss) => ss.iter().any(|s| stmt_calls(s, names)),
        StmtKind::VarDecl { decls } => decls
            .iter()
            .any(|d| d.init.as_ref().is_some_and(|e| expr_calls(e, names))),
        StmtKind::If { cond, then, else_ } => {
            expr_calls(cond, names)
                || stmt_calls(then, names)
                || else_.as_ref().is_some_and(|s| stmt_calls(s, names))
        }
        StmtKind::While { cond, body } | StmtKind::Switch { cond, body } => {
            expr_calls(cond, names) || stmt_calls(body, names)
        }
        StmtKind::DoWhile { body, cond } => stmt_calls(body, names) || expr_calls(cond, names),
        StmtKind::For {
            init,
            cond,
            step,
            body,
        } => {
            init.as_ref().is_some_and(|s| stmt_calls(s, names))
                || cond.as_ref().is_some_and(|e| expr_calls(e, names))
                || step.as_ref().is_some_and(|e| expr_calls(e, names))
                || stmt_calls(body, names)
        }
        StmtKind::Case { lo, hi } => {
            expr_calls(lo, names) || hi.as_ref().is_some_and(|e| expr_calls(e, names))
        }
        StmtKind::Return(e) => e.as_ref().is_some_and(|e| expr_calls(e, names)),
        StmtKind::Func(f) => f
            .body
            .as_ref()
            .is_some_and(|b| b.iter().any(|s| stmt_calls(s, names))),
        StmtKind::TypeSwitch { on, arms, default } => {
            matches!(on, TypeSwitchOn::Val(e) if expr_calls(e, names))
                || arms
                    .iter()
                    .any(|(_, b)| b.iter().any(|s| stmt_calls(s, names)))
                || default
                    .as_ref()
                    .is_some_and(|b| b.iter().any(|s| stmt_calls(s, names)))
        }
        StmtKind::Try { body, handler } => {
            body.iter().any(|s| stmt_calls(s, names))
                || handler.iter().any(|s| stmt_calls(s, names))
        }
        StmtKind::Throw(e) => e.as_ref().is_some_and(|e| expr_calls(e, names)),
    }
}

fn expr_calls(e: &Expr, names: &[&str]) -> bool {
    match &e.kind {
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Str(_)
        | ExprKind::Char(_)
        | ExprKind::Ident(_)
        | ExprKind::Sizeof(_)
        | ExprKind::Offset { .. } => false,
        ExprKind::Call { callee, args } => {
            matches!(&callee.kind, ExprKind::Ident(n) if names.contains(&n.as_str()))
                || expr_calls(callee, names)
                || args.iter().any(|a| expr_calls(a, names))
        }
        ExprKind::GenericCall { name, args, .. } => {
            names.contains(&name.as_str()) || args.iter().any(|a| expr_calls(a, names))
        }
        ExprKind::Unary { expr, .. }
        | ExprKind::Postfix { expr, .. }
        | ExprKind::Cast { expr, .. } => expr_calls(expr, names),
        ExprKind::Binary { lhs, rhs, .. } => expr_calls(lhs, names) || expr_calls(rhs, names),
        ExprKind::Assign { target, value, .. } => {
            expr_calls(target, names) || expr_calls(value, names)
        }
        ExprKind::Ternary { cond, then, else_ } => {
            expr_calls(cond, names) || expr_calls(then, names) || expr_calls(else_, names)
        }
        ExprKind::Index { base, index } => expr_calls(base, names) || expr_calls(index, names),
        ExprKind::Member { base, .. } => expr_calls(base, names),
        ExprKind::InitList(es) | ExprKind::Comma(es) => es.iter().any(|e| expr_calls(e, names)),
        ExprKind::DesignatedInit(fs) => fs.iter().any(|(_, e)| expr_calls(e, names)),
    }
}

/// Whether the program references any of `names` as a bare identifier. Used to
/// decide whether to capture the command line for the implicit `argc`/`argv`.
pub fn program_uses_ident(program: &Program, names: &[&str]) -> bool {
    program.items.iter().any(|s| stmt_uses_ident(s, names))
}

/// Whether the program contains any `try`/`throw`. The backends use this (with a
/// reference to `Fs`) to decide whether to set up the `CTask`/exception machinery.
pub fn program_has_exceptions(program: &Program) -> bool {
    program.items.iter().any(stmt_has_exceptions)
}

/// Whether any of `stmts` references `Fs` or contains a `try`/`throw`. Backends use
/// this per function to decide whether to set up that function's `Fs` access.
pub fn stmts_use_fs_or_exceptions(stmts: &[&Stmt]) -> bool {
    stmts
        .iter()
        .any(|s| stmt_uses_ident(s, &["Fs"]) || stmt_has_exceptions(s))
}

fn stmt_has_exceptions(s: &Stmt) -> bool {
    match &s.kind {
        StmtKind::Try { .. } | StmtKind::Throw(_) => true,
        StmtKind::Block(ss) => ss.iter().any(stmt_has_exceptions),
        StmtKind::If { then, else_, .. } => {
            stmt_has_exceptions(then) || else_.as_deref().is_some_and(stmt_has_exceptions)
        }
        StmtKind::While { body, .. }
        | StmtKind::DoWhile { body, .. }
        | StmtKind::For { body, .. }
        | StmtKind::Switch { body, .. } => stmt_has_exceptions(body),
        StmtKind::Func(f) => f
            .body
            .as_ref()
            .is_some_and(|b| b.iter().any(stmt_has_exceptions)),
        _ => false,
    }
}

fn stmt_uses_ident(s: &Stmt, names: &[&str]) -> bool {
    match &s.kind {
        StmtKind::Empty
        | StmtKind::Default
        | StmtKind::SwitchStart
        | StmtKind::SwitchEnd
        | StmtKind::Break
        | StmtKind::Continue
        | StmtKind::Goto(_)
        | StmtKind::Label(_)
        | StmtKind::Class(_)
        | StmtKind::Include(_) => false,
        StmtKind::Expr(e) => expr_uses_ident(e, names),
        StmtKind::ShortDecl { rhs, .. } => expr_uses_ident(rhs, names),
        StmtKind::Block(ss) => ss.iter().any(|s| stmt_uses_ident(s, names)),
        StmtKind::VarDecl { decls } => decls
            .iter()
            .any(|d| d.init.as_ref().is_some_and(|e| expr_uses_ident(e, names))),
        StmtKind::If { cond, then, else_ } => {
            expr_uses_ident(cond, names)
                || stmt_uses_ident(then, names)
                || else_.as_ref().is_some_and(|s| stmt_uses_ident(s, names))
        }
        StmtKind::While { cond, body } | StmtKind::Switch { cond, body } => {
            expr_uses_ident(cond, names) || stmt_uses_ident(body, names)
        }
        StmtKind::DoWhile { body, cond } => {
            stmt_uses_ident(body, names) || expr_uses_ident(cond, names)
        }
        StmtKind::For {
            init,
            cond,
            step,
            body,
        } => {
            init.as_ref().is_some_and(|s| stmt_uses_ident(s, names))
                || cond.as_ref().is_some_and(|e| expr_uses_ident(e, names))
                || step.as_ref().is_some_and(|e| expr_uses_ident(e, names))
                || stmt_uses_ident(body, names)
        }
        StmtKind::Case { lo, hi } => {
            expr_uses_ident(lo, names) || hi.as_ref().is_some_and(|e| expr_uses_ident(e, names))
        }
        StmtKind::Return(e) => e.as_ref().is_some_and(|e| expr_uses_ident(e, names)),
        StmtKind::Func(f) => f
            .body
            .as_ref()
            .is_some_and(|b| b.iter().any(|s| stmt_uses_ident(s, names))),
        StmtKind::TypeSwitch { on, arms, default } => {
            matches!(on, TypeSwitchOn::Val(e) if expr_uses_ident(e, names))
                || arms
                    .iter()
                    .any(|(_, b)| b.iter().any(|s| stmt_uses_ident(s, names)))
                || default
                    .as_ref()
                    .is_some_and(|b| b.iter().any(|s| stmt_uses_ident(s, names)))
        }
        StmtKind::Try { body, handler } => {
            body.iter().any(|s| stmt_uses_ident(s, names))
                || handler.iter().any(|s| stmt_uses_ident(s, names))
        }
        StmtKind::Throw(e) => e.as_ref().is_some_and(|e| expr_uses_ident(e, names)),
    }
}

fn expr_uses_ident(e: &Expr, names: &[&str]) -> bool {
    match &e.kind {
        ExprKind::Ident(n) => names.contains(&n.as_str()),
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Str(_)
        | ExprKind::Char(_)
        | ExprKind::Sizeof(_)
        | ExprKind::Offset { .. } => false,
        ExprKind::Call { callee, args } => {
            expr_uses_ident(callee, names) || args.iter().any(|a| expr_uses_ident(a, names))
        }
        ExprKind::GenericCall { name, args, .. } => {
            names.contains(&name.as_str()) || args.iter().any(|a| expr_uses_ident(a, names))
        }
        ExprKind::Unary { expr, .. }
        | ExprKind::Postfix { expr, .. }
        | ExprKind::Cast { expr, .. } => expr_uses_ident(expr, names),
        ExprKind::Binary { lhs, rhs, .. } => {
            expr_uses_ident(lhs, names) || expr_uses_ident(rhs, names)
        }
        ExprKind::Assign { target, value, .. } => {
            expr_uses_ident(target, names) || expr_uses_ident(value, names)
        }
        ExprKind::Ternary { cond, then, else_ } => {
            expr_uses_ident(cond, names)
                || expr_uses_ident(then, names)
                || expr_uses_ident(else_, names)
        }
        ExprKind::Index { base, index } => {
            expr_uses_ident(base, names) || expr_uses_ident(index, names)
        }
        ExprKind::Member { base, .. } => expr_uses_ident(base, names),
        ExprKind::InitList(es) | ExprKind::Comma(es) => {
            es.iter().any(|e| expr_uses_ident(e, names))
        }
        ExprKind::DesignatedInit(fs) => fs.iter().any(|(_, e)| expr_uses_ident(e, names)),
    }
}

/// Whether `name` is a compiler-synthesized tuple struct (`$Tup…`). A tuple type
/// `(T1, …, Tn)` is lowered to a positional struct with fields `_0`, `_1`, ….
pub fn is_tuple_name(name: &str) -> bool {
    name.starts_with("$Tup")
}

/// If `e` is `tuple[k]` (a tuple-typed base with a constant index), returns the
/// equivalent member access `tuple._k`, carrying `e`'s already-inferred slot type.
/// Tuple indexing is positional field access, so every backend rewrites it this way.
pub fn tuple_index_as_member(e: &Expr) -> Option<Expr> {
    let ExprKind::Index { base, index } = &e.kind else {
        return None;
    };
    let Some(Type::Named(name)) = base.ty() else {
        return None;
    };
    let ExprKind::Int(k) = &index.kind else {
        return None;
    };
    if !is_tuple_name(&name) || *k < 0 {
        return None;
    }
    let m = Expr::new(
        ExprKind::Member {
            base: base.clone(),
            field: format!("_{k}"),
            arrow: false,
        },
        e.span,
    );
    if let Some(t) = e.ty() {
        m.set_ty(t);
    }
    Some(m)
}
