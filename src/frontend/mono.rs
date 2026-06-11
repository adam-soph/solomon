//! The `mono` pass: monomorphization. It runs after parsing and before
//! sema/layout/codegen, resolving the generic constructs the parser defers into
//! ordinary concrete AST.
//!
//! The parser never instantiates anything itself. Instead it emits four deferred
//! node kinds:
//!
//! * `Type::Generic(name, args)` — a generic-class use (`Vec<I64>`),
//! * `Type::Tuple(elems)` — a tuple type (`(I64, F64)`),
//! * `ExprKind::GenericCall { name, type_args, args }` — a generic-function call
//!   (explicit `Id<I64>(x)` or inferred `Id(x)`),
//! * `StmtKind::ShortDecl { names, rhs }` — a `:=` short declaration / unpack.
//!
//! This pass owns the monomorphization worklist. It also includes a full scoped,
//! recursive typer ([`Mono::type_expr`]) over the already-parsed program. Running
//! after the full parse means it sees forward declarations, ternaries,
//! function-pointer results, and inherited (base-class) fields. That closes the
//! gaps of the old parse-time `arg_type` inference, which could only see types
//! declared so far.
//!
//! The synthetic definitions it generates — instantiated classes/functions,
//! interned tuple structs, unpack temporaries — are stamped with the
//! `GENERATED_FILE` sentinel span. Sema treats references originating from generated
//! code as trusted, so file-scoped visibility never rejects a monomorphized instance
//! (e.g. `Vec<Pt>` over a user's non-`public` `Pt`).

use crate::ast::*;
use crate::parser::{mangle_generic, tuple_type_name};
use crate::token::{Pos, Span};
use std::collections::{HashMap, HashSet};

/// An error raised while monomorphizing: an un-inferable type argument, a generic
/// arity mismatch, or a non-inferable `:=`. Its shape mirrors `ParseError` so the
/// CLIs can report it the same way.
#[derive(Debug, Clone)]
pub struct MonoError {
    pub message: String,
    pub pos: Pos,
}

type MResult<T> = Result<T, MonoError>;

/// A bound generic parameter during instantiation: a concrete type (for a `type`/
/// `comparable` parameter) or an integer (for an `int` value parameter).
#[derive(Clone, Debug)]
enum Bind {
    Type(Type),
    Value(i64),
}

/// Build the substitution environment for one instantiation from a template's
/// parameters and its (already-normalized) arguments. Value args have been
/// const-evaluated to `Int` literals by resolution time, so this just reads them.
fn binds_of(params: &[GenericParam], args: &[GenericArg]) -> HashMap<String, Bind> {
    params
        .iter()
        .zip(args.iter())
        .filter_map(|(p, a)| match (p, a) {
            (GenericParam::Type(n, _), GenericArg::Type(t)) => {
                Some((n.clone(), Bind::Type(t.clone())))
            }
            (GenericParam::Value(n), GenericArg::Value(e)) => match e.kind {
                ExprKind::Int(v) => Some((n.clone(), Bind::Value(v))),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// A readable type name for diagnostics (e.g. the `comparable` constraint error).
fn type_name(t: &Type) -> String {
    match t {
        Type::Named(n) => n.clone(),
        Type::Ptr(inner) => format!("{} *", type_name(inner)),
        Type::Array(inner, _) => format!("{}[]", type_name(inner)),
        other => format!("{other:?}"),
    }
}

/// Expand every deferred generic construct in `program`, returning a fully concrete
/// program. No `Type::Generic`, `Type::Tuple`, `GenericCall`, or `ShortDecl`
/// remains, so sema, layout, the interpreter, and the backends only ever see
/// ordinary AST.
pub fn expand(mut program: Program) -> MResult<Program> {
    let templates = std::mem::take(&mut program.generics);
    let mut m = Mono::new(templates);
    let mut items = std::mem::take(&mut program.items);
    m.run(&mut items)?;
    items.append(&mut m.generated);
    program.items = items;
    Ok(program)
}

/// A synthetic span for generated definitions, stamped with the `GENERATED_FILE`
/// sentinel so sema treats references originating from monomorphized code as trusted
/// (always visible).
fn syn_span() -> Span {
    let mut s = Span::dummy();
    s.file = crate::token::GENERATED_FILE;
    s
}

struct Mono {
    /// Generic `class`/`union` templates, by name (parsed once, parameters symbolic).
    classes: HashMap<String, GenericClass>,
    /// Generic function templates, by name.
    fns: HashMap<String, GenericFn>,
    /// Mangled names of class instances already generated. Deduplicates instantiation.
    class_done: HashSet<String>,
    /// Mangled names of function instances already generated. Deduplicates instantiation.
    fn_done: HashSet<String>,
    /// Class-instance mangled name -> `(template, args)`. Lets a `Vec<T>`
    /// parameter unify against a `Vec_I64` argument to bind `T`.
    instances: HashMap<String, (String, Vec<GenericArg>)>,
    /// Concrete class/union/tuple field types, by type name, for the typer.
    class_fields: HashMap<String, Vec<(String, Type)>>,
    /// Each concrete class/union's base class, for inherited-field lookup in the typer.
    class_bases: HashMap<String, Option<String>>,
    /// Concrete function/instance return types, for the typer.
    fn_rets: HashMap<String, Type>,
    /// Canonical names of tuple structs already interned. Deduplicates interning.
    tuple_types: HashSet<String>,
    /// Pending class instantiations `(template, args)`.
    pending_classes: Vec<(String, Vec<GenericArg>)>,
    /// Pending function instantiations `(template, args)`.
    pending_fns: Vec<(String, Vec<GenericArg>)>,
    /// Generated concrete definitions: instances and interned tuples. Appended to the
    /// program's items at the end.
    generated: Vec<Stmt>,
    /// Lexical scope stack of variable types, for the typer (innermost last).
    scopes: Vec<HashMap<String, Type>>,
}

impl Mono {
    fn new(templates: GenericTemplates) -> Self {
        Mono {
            classes: templates.classes,
            fns: templates.fns,
            class_done: HashSet::new(),
            fn_done: HashSet::new(),
            instances: HashMap::new(),
            class_fields: HashMap::new(),
            class_bases: HashMap::new(),
            fn_rets: HashMap::new(),
            tuple_types: HashSet::new(),
            pending_classes: Vec::new(),
            pending_fns: Vec::new(),
            generated: Vec::new(),
            scopes: Vec::new(),
        }
    }

    fn err<T>(&self, pos: Pos, msg: impl Into<String>) -> MResult<T> {
        Err(MonoError {
            message: msg.into(),
            pos,
        })
    }

    /// Resolve the whole top-level item list in place, then drain the instantiation
    /// worklists to a fixpoint.
    fn run(&mut self, items: &mut [Stmt]) -> MResult<()> {
        self.scopes.push(HashMap::new()); // global scope
        // Pass 1: resolve and record all top-level signatures (function return type
        // and params, class fields). This lets forward references resolve when
        // bodies are processed in pass 2.
        for it in items.iter_mut() {
            self.resolve_sig(it)?;
        }
        // Pass 2: resolve top-level bodies and statements.
        for it in items.iter_mut() {
            self.resolve_top(it)?;
        }
        self.drain()?;
        Ok(())
    }

    /// Resolve and record a top-level definition's signature (no body).
    fn resolve_sig(&mut self, s: &mut Stmt) -> MResult<()> {
        match &mut s.kind {
            StmtKind::Func(f) => {
                f.ret = self.resolve_type(&f.ret, s.span.pos)?;
                for p in &mut f.params {
                    p.ty = self.resolve_type(&p.ty, s.span.pos)?;
                }
                self.fn_rets.insert(f.name.clone(), f.ret.clone());
            }
            StmtKind::Class(c) => {
                for fld in &mut c.fields {
                    fld.ty = self.resolve_type(&fld.ty, fld.span.pos)?;
                }
                self.class_bases.insert(c.name.clone(), c.base.clone());
                self.class_fields.insert(
                    c.name.clone(),
                    c.fields
                        .iter()
                        .map(|d| (d.name.clone(), d.ty.clone()))
                        .collect(),
                );
            }
            _ => {}
        }
        Ok(())
    }

    /// Resolve a top-level item's body or executable part. A function gets its own
    /// scope for its parameters; everything else is an ordinary top-level statement.
    fn resolve_top(&mut self, s: &mut Stmt) -> MResult<()> {
        match &mut s.kind {
            StmtKind::Func(f) => {
                self.scopes.push(HashMap::new());
                for p in &f.params {
                    if let Some(pname) = &p.name {
                        self.scope_insert(pname.clone(), p.ty.clone());
                    }
                }
                // Resolve any parameter default expressions, in the function scope.
                let pos = s.span.pos;
                for p in &mut f.params {
                    if let Some(d) = &mut p.default {
                        self.resolve_expr(d)?;
                    }
                }
                let _ = pos;
                if let Some(body) = &mut f.body {
                    for st in body.iter_mut() {
                        self.resolve_stmt(st)?;
                    }
                }
                self.scopes.pop();
                Ok(())
            }
            StmtKind::Class(_) => Ok(()), // fields already resolved in resolve_sig
            _ => self.resolve_stmt(s),
        }
    }

    // ---- statement / expression resolution ----

    fn resolve_stmt(&mut self, s: &mut Stmt) -> MResult<()> {
        match &mut s.kind {
            StmtKind::Empty
            | StmtKind::Default
            | StmtKind::SwitchStart
            | StmtKind::SwitchEnd
            | StmtKind::Break
            | StmtKind::Continue
            | StmtKind::Goto(_)
            | StmtKind::Label(_)
            | StmtKind::Include(_) => Ok(()),
            StmtKind::Expr(e) => self.resolve_expr(e),
            StmtKind::Return(e) => {
                if let Some(e) = e {
                    self.resolve_expr(e)?;
                }
                Ok(())
            }
            StmtKind::Block(ss) => {
                self.scopes.push(HashMap::new());
                for st in ss.iter_mut() {
                    self.resolve_stmt(st)?;
                }
                self.scopes.pop();
                Ok(())
            }
            StmtKind::VarDecl { decls } => {
                for d in decls.iter_mut() {
                    d.ty = self.resolve_type(&d.ty, d.span.pos)?;
                    if let Some(init) = &mut d.init {
                        self.resolve_expr(init)?;
                    }
                    self.scope_insert(d.name.clone(), d.ty.clone());
                }
                Ok(())
            }
            StmtKind::ShortDecl { .. } => {
                let (names, mut rhs) = match std::mem::replace(&mut s.kind, StmtKind::Empty) {
                    StmtKind::ShortDecl { names, rhs } => (names, rhs),
                    _ => unreachable!(),
                };
                self.resolve_expr(&mut rhs)?;
                let kind = self.build_unpack(names, rhs, s.span)?;
                if let StmtKind::VarDecl { decls } = &kind {
                    for d in decls {
                        self.scope_insert(d.name.clone(), d.ty.clone());
                    }
                }
                s.kind = kind;
                Ok(())
            }
            StmtKind::If { cond, then, else_ } => {
                self.resolve_expr(cond)?;
                self.resolve_stmt(then)?;
                if let Some(e) = else_ {
                    self.resolve_stmt(e)?;
                }
                Ok(())
            }
            StmtKind::While { cond, body } => {
                self.resolve_expr(cond)?;
                self.resolve_stmt(body)
            }
            StmtKind::DoWhile { body, cond } => {
                self.resolve_stmt(body)?;
                self.resolve_expr(cond)
            }
            StmtKind::For {
                init,
                cond,
                step,
                body,
            } => {
                self.scopes.push(HashMap::new()); // the init may declare a loop variable
                if let Some(i) = init {
                    self.resolve_stmt(i)?;
                }
                if let Some(c) = cond {
                    self.resolve_expr(c)?;
                }
                if let Some(st) = step {
                    self.resolve_expr(st)?;
                }
                self.resolve_stmt(body)?;
                self.scopes.pop();
                Ok(())
            }
            StmtKind::Switch { cond, body } => {
                self.resolve_expr(cond)?;
                self.resolve_stmt(body)
            }
            StmtKind::Case { lo, hi } => {
                self.resolve_expr(lo)?;
                if let Some(h) = hi {
                    self.resolve_expr(h)?;
                }
                Ok(())
            }
            // A compile-time type switch: resolve the scrutinee's type, keep the
            // matching arm (or `default`), and drop the rest. The unselected arms are
            // never resolved or sema-checked.
            StmtKind::TypeSwitch { .. } => {
                let (on, arms, default) = match std::mem::replace(&mut s.kind, StmtKind::Empty) {
                    StmtKind::TypeSwitch { on, arms, default } => (on, arms, default),
                    _ => unreachable!(),
                };
                let pos = s.span.pos;
                let scrut = match on {
                    TypeSwitchOn::Ty(t) => self.resolve_type(&t, pos)?,
                    TypeSwitchOn::Val(e) => {
                        let mut ex = *e;
                        self.resolve_expr(&mut ex)?;
                        match self.type_expr(&ex) {
                            Some(t) => self.resolve_type(&t, pos)?,
                            None => {
                                return self.err(
                                    pos,
                                    "type switch: cannot determine the type of the scrutinee",
                                );
                            }
                        }
                    }
                };
                let mut chosen: Option<Vec<Stmt>> = None;
                for (casety, body) in arms {
                    if self.resolve_type(&casety, pos)? == scrut {
                        chosen = Some(body);
                        break;
                    }
                }
                match chosen.or(default) {
                    // Re-run as a block so the chosen arm gets its own scope and its
                    // statements are resolved normally.
                    Some(body) => {
                        s.kind = StmtKind::Block(body);
                        self.resolve_stmt(s)?;
                    }
                    None => s.kind = StmtKind::Empty,
                }
                Ok(())
            }
            StmtKind::Try { body, handler } => {
                self.scopes.push(HashMap::new());
                for st in body.iter_mut() {
                    self.resolve_stmt(st)?;
                }
                self.scopes.pop();
                self.scopes.push(HashMap::new());
                for st in handler.iter_mut() {
                    self.resolve_stmt(st)?;
                }
                self.scopes.pop();
                Ok(())
            }
            StmtKind::Throw(e) => {
                if let Some(e) = e {
                    self.resolve_expr(e)?;
                }
                Ok(())
            }
            // Nested function/class definitions aren't supported; sema and the
            // backends reject them. `mono` still resolves them rather than gating
            // them here, leaving the rejection to those later passes.
            StmtKind::Func(_) | StmtKind::Class(_) => {
                self.resolve_sig(s)?;
                self.resolve_top(s)
            }
        }
    }

    fn resolve_expr(&mut self, e: &mut Expr) -> MResult<()> {
        let pos = e.span.pos;
        match &mut e.kind {
            ExprKind::Int(_)
            | ExprKind::Float(_)
            | ExprKind::Str(_)
            | ExprKind::Char(_)
            | ExprKind::Ident(_)
            | ExprKind::Offset { .. } => Ok(()),
            ExprKind::Unary { expr, .. } | ExprKind::Postfix { expr, .. } => {
                self.resolve_expr(expr)
            }
            ExprKind::Binary { lhs, rhs, .. } => {
                self.resolve_expr(lhs)?;
                self.resolve_expr(rhs)
            }
            ExprKind::Assign { target, value, .. } => {
                self.resolve_expr(target)?;
                self.resolve_expr(value)
            }
            ExprKind::Ternary { cond, then, else_ } => {
                self.resolve_expr(cond)?;
                self.resolve_expr(then)?;
                self.resolve_expr(else_)
            }
            ExprKind::Call { callee, args } => {
                self.resolve_expr(callee)?;
                for a in args.iter_mut() {
                    self.resolve_expr(a)?;
                }
                Ok(())
            }
            ExprKind::GenericCall { .. } => {
                let (name, type_args, mut args) =
                    match std::mem::replace(&mut e.kind, ExprKind::Int(0)) {
                        ExprKind::GenericCall {
                            name,
                            type_args,
                            args,
                        } => (name, type_args, args),
                        _ => unreachable!(),
                    };
                // Resolve the value arguments first (they may contain nested generics).
                for a in args.iter_mut() {
                    self.resolve_expr(a)?;
                }
                let params = match self.fns.get(&name) {
                    Some(f) => f.params.clone(),
                    None => return self.err(pos, format!("`{name}` is not a generic function")),
                };
                // With explicit `<...>` args, resolve them per the parameter kinds.
                // Otherwise infer the type parameters from the argument types — but a
                // value (`int`) parameter can't be inferred, so it must be explicit.
                let cargs = if type_args.is_empty() {
                    if params.iter().any(|p| matches!(p, GenericParam::Value(_))) {
                        return self.err(
                            pos,
                            format!(
                                "generic function `{name}` has value parameter(s); \
                                 call it explicitly as `{name}<...>(...)`"
                            ),
                        );
                    }
                    self.infer(&name, &args, pos)?
                } else {
                    self.resolve_args(&name, &params, &type_args, pos)?
                };
                let mangled = mangle_generic(&name, &cargs);
                self.record_instance_ret(&name, &cargs, &mangled, pos)?;
                self.pending_fns.push((name, cargs));
                e.kind = ExprKind::Call {
                    callee: Box::new(Expr::new(ExprKind::Ident(mangled), e.span)),
                    args,
                };
                Ok(())
            }
            ExprKind::Index { base, index } => {
                self.resolve_expr(base)?;
                self.resolve_expr(index)
            }
            ExprKind::Member { base, .. } => self.resolve_expr(base),
            ExprKind::Cast { ty, expr } => {
                *ty = self.resolve_type(ty, pos)?;
                self.resolve_expr(expr)
            }
            ExprKind::Sizeof(SizeofArg::Type(t)) => {
                *t = self.resolve_type(t, pos)?;
                Ok(())
            }
            ExprKind::Sizeof(SizeofArg::Expr(ex)) => self.resolve_expr(ex),
            ExprKind::InitList(items) | ExprKind::Comma(items) => {
                for it in items.iter_mut() {
                    self.resolve_expr(it)?;
                }
                Ok(())
            }
            ExprKind::DesignatedInit(pairs) => {
                for (_, ex) in pairs.iter_mut() {
                    self.resolve_expr(ex)?;
                }
                Ok(())
            }
        }
    }

    /// Resolve a type. A `Type::Generic` instantiates its class — queuing it — and
    /// becomes the mangled `Named`. A `Type::Tuple` interns its struct. Pointer,
    /// array, and function-pointer wrappers recurse into their inner types.
    fn resolve_type(&mut self, ty: &Type, pos: Pos) -> MResult<Type> {
        Ok(match ty {
            Type::Generic(name, args) => {
                let params = match self.classes.get(name) {
                    Some(c) => c.params.clone(),
                    None => return self.err(pos, format!("`{name}` is not a generic type")),
                };
                let cargs = self.resolve_args(name, &params, args, pos)?;
                self.instantiate_class_ref(name, cargs, pos)?
            }
            Type::Tuple(elems) => {
                let celems = elems
                    .iter()
                    .map(|a| self.resolve_type(a, pos))
                    .collect::<MResult<Vec<_>>>()?;
                Type::Named(self.intern_tuple(&celems))
            }
            Type::Ptr(inner) => Type::Ptr(Box::new(self.resolve_type(inner, pos)?)),
            Type::Array(inner, dim) => {
                // The dimension expression can itself embed a deferred generic/tuple type —
                // e.g. `T data[sizeof(Vec<U>)]` — so resolve it too, not just the element
                // type; an unresolved `Generic`/`Tuple` left in the dim survives to `layout`
                // and trips its `unreachable!`.
                let dim = match dim {
                    Some(d) => {
                        let mut d = d.clone();
                        self.resolve_expr(&mut d)?;
                        Some(d)
                    }
                    None => None,
                };
                Type::Array(Box::new(self.resolve_type(inner, pos)?), dim)
            }
            Type::FuncPtr { ret, params } => Type::FuncPtr {
                ret: Box::new(self.resolve_type(ret, pos)?),
                params: params
                    .iter()
                    .map(|p| self.resolve_type(p, pos))
                    .collect::<MResult<Vec<_>>>()?,
            },
            Type::Param(n) => {
                return self.err(
                    pos,
                    format!("unbound type parameter `{n}` outside a template"),
                );
            }
            other => other.clone(),
        })
    }

    /// Resolve a generic argument list against a template's parameter kinds: each
    /// position parses a type or a value, matching the parameter. Type args are
    /// resolved (instantiating any nested generics) and `comparable` constraints are
    /// enforced; value args are const-evaluated to an `Int` literal.
    fn resolve_args(
        &mut self,
        name: &str,
        params: &[GenericParam],
        args: &[GenericArg],
        pos: Pos,
    ) -> MResult<Vec<GenericArg>> {
        if args.len() != params.len() {
            return self.err(
                pos,
                format!(
                    "generic `{name}` expects {} argument(s), got {}",
                    params.len(),
                    args.len()
                ),
            );
        }
        let mut out = Vec::with_capacity(args.len());
        for (p, a) in params.iter().zip(args.iter()) {
            out.push(self.resolve_arg(name, p, a, pos)?);
        }
        Ok(out)
    }

    /// Resolve one generic argument against its parameter. Reports a clear error on a
    /// kind mismatch (a value where a type is expected, or vice versa) or an unmet
    /// `comparable` constraint.
    fn resolve_arg(
        &mut self,
        gen_name: &str,
        param: &GenericParam,
        arg: &GenericArg,
        pos: Pos,
    ) -> MResult<GenericArg> {
        match (param, arg) {
            (GenericParam::Type(_, constraint), GenericArg::Type(t)) => {
                let rt = self.resolve_type(t, pos)?;
                if *constraint == Some(Constraint::Comparable) && !crate::sema::is_scalar(&rt) {
                    return self.err(
                        pos,
                        format!(
                            "type argument `{}` for parameter `{}` of `{gen_name}` is not \
                             comparable (needs a scalar or pointer type)",
                            type_name(&rt),
                            param.name()
                        ),
                    );
                }
                Ok(GenericArg::Type(rt))
            }
            (GenericParam::Value(_), GenericArg::Value(e)) => {
                let v = crate::layout::const_eval(e).map_err(|le| MonoError {
                    message: le.message,
                    pos: le.pos,
                })?;
                Ok(GenericArg::Value(Box::new(Expr::new(
                    ExprKind::Int(v),
                    syn_span(),
                ))))
            }
            (GenericParam::Type(..), GenericArg::Value(_)) => self.err(
                pos,
                format!(
                    "parameter `{}` of `{gen_name}` expects a type, not a value",
                    param.name()
                ),
            ),
            (GenericParam::Value(_), GenericArg::Type(_)) => self.err(
                pos,
                format!(
                    "parameter `{}` of `{gen_name}` expects a value, not a type",
                    param.name()
                ),
            ),
        }
    }

    /// Record a class instantiation request, deduped, and return its mangled `Named`
    /// type. The arguments are already resolved/validated. The concrete definition is
    /// generated later, in [`Mono::drain`].
    fn instantiate_class_ref(
        &mut self,
        name: &str,
        args: Vec<GenericArg>,
        _pos: Pos,
    ) -> MResult<Type> {
        let mangled = mangle_generic(name, &args);
        self.instances
            .insert(mangled.clone(), (name.to_string(), args.clone()));
        if self.class_done.insert(mangled.clone()) {
            self.pending_classes.push((name.to_string(), args));
        }
        Ok(Type::Named(mangled))
    }

    /// Intern the canonical tuple struct for `elems` and return its type name. The
    /// synthetic `class $Tup…` is generated only on the first request.
    fn intern_tuple(&mut self, elems: &[Type]) -> String {
        let name = tuple_type_name(elems);
        if self.tuple_types.insert(name.clone()) {
            let pairs: Vec<(String, Type)> = elems
                .iter()
                .enumerate()
                .map(|(i, t)| (format!("_{i}"), t.clone()))
                .collect();
            self.class_fields.insert(name.clone(), pairs.clone());
            self.class_bases.insert(name.clone(), None);
            let fields = pairs
                .into_iter()
                .map(|(n, t)| Declarator {
                    name: n,
                    ty: t,
                    init: None,
                    span: syn_span(),
                    is_public: false,
                })
                .collect();
            self.generated.push(Stmt::new(
                StmtKind::Class(ClassDef {
                    is_union: false,
                    name: name.clone(),
                    base: None,
                    fields,
                    // Synthetic tuple aggregate: never privacy-gated.
                    is_public: true,
                }),
                syn_span(),
            ));
        }
        name
    }

    // ---- instantiation (parameter substitution + resolution) ----

    /// Drain the instantiation worklists to a fixpoint. Generating one instance can
    /// request more class or function instances, since a body may use or call other
    /// generics. The loop runs until both queues are empty.
    fn drain(&mut self) -> MResult<()> {
        let mut guard = 0usize;
        loop {
            guard += 1;
            if guard > 5_000_000 {
                return self.err(Pos::new(0, 0), "generic instantiation did not terminate");
            }
            if let Some((name, args)) = self.pending_classes.pop() {
                let stmt = self.instantiate_class(&name, &args)?;
                self.generated.push(stmt);
            } else if let Some((name, args)) = self.pending_fns.pop() {
                let mangled = mangle_generic(&name, &args);
                if !self.fn_done.insert(mangled.clone()) {
                    continue;
                }
                let stmt = self.instantiate_fn(&name, &args, &mangled)?;
                self.generated.push(stmt);
            } else {
                break;
            }
        }
        Ok(())
    }

    fn instantiate_class(&mut self, name: &str, args: &[GenericArg]) -> MResult<Stmt> {
        let tmpl = self.classes.get(name).expect("generic class").clone();
        let binds = binds_of(&tmpl.params, args);
        let mangled = mangle_generic(name, args);
        let mut fields = Vec::with_capacity(tmpl.fields.len());
        for f in &tmpl.fields {
            fields.push(Declarator {
                name: f.name.clone(),
                ty: subst_type(&f.ty, &binds),
                init: None,
                span: f.span,
                is_public: false,
            });
        }
        let mut stmt = Stmt::new(
            StmtKind::Class(ClassDef {
                is_union: tmpl.is_union,
                name: mangled,
                base: tmpl.base.clone(),
                fields,
                // Monomorphized generic instance: always public (its definition file is
                // synthetic, so it must not be privacy-gated against use sites).
                is_public: true,
            }),
            syn_span(),
        );
        self.resolve_sig(&mut stmt)?; // resolve the substituted field types and record them
        Ok(stmt)
    }

    fn instantiate_fn(&mut self, name: &str, args: &[GenericArg], mangled: &str) -> MResult<Stmt> {
        let tmpl = self.fns.get(name).expect("generic fn").clone();
        if args.len() != tmpl.params.len() {
            return self.err(
                Pos::new(0, 0),
                format!(
                    "generic function `{name}` expects {} argument(s), got {}",
                    tmpl.params.len(),
                    args.len()
                ),
            );
        }
        let binds = binds_of(&tmpl.params, args);
        let params = tmpl
            .def
            .params
            .iter()
            .map(|p| Param {
                ty: subst_type(&p.ty, &binds),
                name: p.name.clone(),
                default: p.default.as_ref().map(|e| subst_expr(e, &binds)),
                span: p.span,
            })
            .collect();
        let body = tmpl
            .def
            .body
            .as_ref()
            .map(|stmts| stmts.iter().map(|s| subst_stmt(s, &binds)).collect());
        let mut stmt = Stmt::new(
            StmtKind::Func(FuncDef {
                ret: subst_type(&tmpl.def.ret, &binds),
                name: mangled.to_string(),
                params,
                varargs: tmpl.def.varargs,
                body,
                // Monomorphized generic instance: always public (see instantiate_class).
                is_public: true,
            }),
            syn_span(),
        );
        self.resolve_sig(&mut stmt)?;
        self.resolve_top(&mut stmt)?;
        Ok(stmt)
    }

    /// Record a generic-function instance's concrete return type, so a call site and
    /// a `:=` can see through it. Substitutes the bound type arguments into the
    /// template's return type, then resolves it.
    fn record_instance_ret(
        &mut self,
        name: &str,
        args: &[GenericArg],
        mangled: &str,
        pos: Pos,
    ) -> MResult<()> {
        let tmpl = self.fns.get(name).cloned().ok_or_else(|| MonoError {
            message: format!("`{name}` is not a generic function"),
            pos,
        })?;
        if args.len() != tmpl.params.len() {
            return self.err(
                pos,
                format!(
                    "generic function `{name}` expects {} argument(s), got {}",
                    tmpl.params.len(),
                    args.len()
                ),
            );
        }
        let binds = binds_of(&tmpl.params, args);
        let ret = self.resolve_type(&subst_type(&tmpl.def.ret, &binds), pos)?;
        self.fn_rets.insert(mangled.to_string(), ret);
        Ok(())
    }

    // ---- call-site type-argument inference ----

    /// Infer a generic function's type arguments from its resolved argument
    /// expressions. Each parameter's template type is unified against the argument's
    /// static type.
    fn infer(&self, name: &str, args: &[Expr], pos: Pos) -> MResult<Vec<GenericArg>> {
        let tmpl = self.fns.get(name).ok_or_else(|| MonoError {
            message: format!("`{name}` is not a generic function"),
            pos,
        })?;
        let mut binds: HashMap<String, Type> = HashMap::new();
        for (i, p) in tmpl.def.params.iter().enumerate() {
            if let Some(arg) = args.get(i) {
                if let Some(aty) = self.type_expr(arg) {
                    self.unify(&p.ty, &aty, &mut binds);
                }
            }
        }
        let mut out = Vec::with_capacity(tmpl.params.len());
        for tp in &tmpl.params {
            match tp {
                GenericParam::Type(n, constraint) => match binds.get(n) {
                    Some(t) => {
                        if *constraint == Some(Constraint::Comparable) && !crate::sema::is_scalar(t)
                        {
                            return self.err(
                                pos,
                                format!(
                                    "inferred type argument `{}` for parameter `{n}` of `{name}` \
                                     is not comparable (needs a scalar or pointer type)",
                                    type_name(t)
                                ),
                            );
                        }
                        out.push(GenericArg::Type(t.clone()));
                    }
                    None => {
                        return self.err(
                            pos,
                            format!(
                                "cannot infer type argument `{n}` for generic `{name}`; \
                                 call it as `{name}<...>(...)`"
                            ),
                        );
                    }
                },
                // The caller rejects inference when any value parameter is present.
                GenericParam::Value(_) => {
                    unreachable!("value parameters require an explicit argument list")
                }
            }
        }
        Ok(out)
    }

    /// The static type of an expression: a complete, scoped, recursive typer over the
    /// resolved program. Returns `None` when a type can't be determined; a generic
    /// call then needs explicit type arguments. Used for type-argument inference and
    /// `:=` declarations.
    fn type_expr(&self, e: &Expr) -> Option<Type> {
        match &e.kind {
            ExprKind::Int(_) | ExprKind::Char(_) => Some(Type::I64),
            ExprKind::Float(_) => Some(Type::F64),
            ExprKind::Str(_) => Some(Type::Ptr(Box::new(Type::U8))),
            ExprKind::Ident(n) => self.lookup_var(n),
            ExprKind::Unary {
                op: UnOp::AddrOf,
                expr,
            } => Some(Type::Ptr(Box::new(self.type_expr(expr)?))),
            ExprKind::Unary {
                op: UnOp::Deref,
                expr,
            } => match self.type_expr(expr)? {
                Type::Ptr(inner) | Type::Array(inner, _) => Some(*inner),
                _ => None,
            },
            // Logical-not yields a boolean-valued I64 regardless of operand type.
            // The other unary/postfix ops (`-`, `~`, `++`, `--`) preserve the type.
            ExprKind::Unary { op: UnOp::Not, .. } => Some(Type::I64),
            ExprKind::Unary { expr, .. } | ExprKind::Postfix { expr, .. } => self.type_expr(expr),
            ExprKind::Cast { ty, .. } => Some(ty.clone()),
            ExprKind::Call { callee, .. } => self.call_ret(callee),
            ExprKind::Member { base, field, .. } => {
                let cname = self.class_name_of(&self.type_expr(base)?)?;
                self.field_type(&cname, field)
            }
            ExprKind::Index { base, .. } => match self.type_expr(base)? {
                Type::Ptr(elem) | Type::Array(elem, _) => Some(*elem),
                _ => None,
            },
            ExprKind::Binary { op, lhs, rhs } => {
                use BinOp::*;
                match op {
                    // Comparisons, logicals, and bitwise/shift ops are I64-valued.
                    // Mirrors sema::check_binary.
                    Eq | Ne | Lt | Gt | Le | Ge | And | Or | BitAnd | BitOr | BitXor | Shl
                    | Shr => Some(Type::I64),
                    // Arithmetic follows the shared `arith_result` rule so pointer
                    // arithmetic types correctly: `arr + 1` is `T*`, not I64.
                    // Pointer-minus-pointer is an integer offset. If an operand
                    // can't be typed yet, fall back to a float-or-int heuristic,
                    // keeping inference as permissive as before.
                    Add | Sub | Mul | Div | Mod => {
                        let lt = self.type_expr(lhs).map(crate::sema::decay);
                        let rt = self.type_expr(rhs).map(crate::sema::decay);
                        match (&lt, &rt) {
                            (Some(a), Some(b)) => {
                                if *op == Sub
                                    && crate::sema::is_pointer(a)
                                    && crate::sema::is_pointer(b)
                                {
                                    Some(Type::I64)
                                } else {
                                    Some(crate::sema::arith_result(a, b))
                                }
                            }
                            _ => {
                                let f =
                                    matches!(lt, Some(Type::F64)) || matches!(rt, Some(Type::F64));
                                Some(if f { Type::F64 } else { Type::I64 })
                            }
                        }
                    }
                }
            }
            // A ternary's type is its arms' common type, via the shared
            // `arith_result` rule, so `c ? 1 : 1.0` is F64, matching sema. If only
            // one arm types, fall back to that arm.
            ExprKind::Ternary { then, else_, .. } => {
                match (self.type_expr(then), self.type_expr(else_)) {
                    (Some(a), Some(b)) => Some(crate::sema::arith_result(&a, &b)),
                    (a, b) => a.or(b),
                }
            }
            ExprKind::Sizeof(_) => Some(Type::I64),
            // `offset(C.f)` is a byte offset (I64); a comma yields its last element's type;
            // an assignment yields its target's type — mirroring `sema::check_expr` so `:=`
            // can infer through these forms instead of bailing to "cannot infer".
            ExprKind::Offset { .. } => Some(Type::I64),
            ExprKind::Comma(items) => items.last().and_then(|e| self.type_expr(e)),
            ExprKind::Assign { target, .. } => self.type_expr(target),
            // `InitList`/`DesignatedInit` are valid only as a variable initializer (sema
            // rejects them as expressions), so they have no inferable `:=` type.
            _ => None,
        }
    }

    /// The return type of a call with the given `callee`. The callee may be a named
    /// function (including a generic instance), or a function-pointer value such as a
    /// variable or a class field.
    fn call_ret(&self, callee: &Expr) -> Option<Type> {
        match &callee.kind {
            ExprKind::Ident(n) => {
                if let Some(ret) = self.fn_rets.get(n) {
                    return Some(ret.clone());
                }
                // Otherwise it may be a function-pointer variable.
                match self.lookup_var(n)? {
                    Type::FuncPtr { ret, .. } => Some(*ret),
                    _ => None,
                }
            }
            // A function-pointer field or call result: type the callee and expect a
            // `FuncPtr`.
            _ => match self.type_expr(callee)? {
                Type::FuncPtr { ret, .. } => Some(*ret),
                _ => None,
            },
        }
    }

    /// The class/union name that a value's type refers to: a `Named`, or a pointer to
    /// one.
    fn class_name_of(&self, ty: &Type) -> Option<String> {
        match ty {
            Type::Named(n) => Some(n.clone()),
            Type::Ptr(inner) => match inner.as_ref() {
                Type::Named(n) => Some(n.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    /// A field's type in class `cname`. Walks the base-class chain to find an
    /// inherited field.
    fn field_type(&self, cname: &str, field: &str) -> Option<Type> {
        let mut cur = Some(cname.to_string());
        while let Some(c) = cur {
            if let Some(fields) = self.class_fields.get(&c) {
                if let Some((_, t)) = fields.iter().find(|(n, _)| n == field) {
                    return Some(t.clone());
                }
            }
            cur = self.class_bases.get(&c).cloned().flatten();
        }
        None
    }

    fn lookup_var(&self, name: &str) -> Option<Type> {
        for scope in self.scopes.iter().rev() {
            if let Some(t) = scope.get(name) {
                return Some(t.clone());
            }
        }
        None
    }

    fn scope_insert(&mut self, name: String, ty: Type) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name, ty);
        }
    }

    /// Unify a template parameter type `pat` against a concrete argument type `ty`,
    /// binding type parameters along the way. A `Generic` parameter such as `Vec<T>`
    /// is matched against a concrete instance argument such as `Vec_I64` through the
    /// instance map.
    fn unify(&self, pat: &Type, ty: &Type, out: &mut HashMap<String, Type>) {
        match pat {
            Type::Param(p) => {
                out.entry(p.clone()).or_insert_with(|| ty.clone());
            }
            Type::Ptr(inner) | Type::Array(inner, _) => match ty {
                Type::Ptr(t) | Type::Array(t, _) => self.unify(inner, t, out),
                _ => {}
            },
            Type::Generic(g, gargs) => {
                if let Type::Named(n) = ty {
                    if let Some((iname, iargs)) = self.instances.get(n) {
                        if iname == g {
                            // Only type positions carry type parameters to bind; value
                            // positions are skipped.
                            for (pa, ta) in gargs.iter().zip(iargs.iter()) {
                                if let (GenericArg::Type(pt), GenericArg::Type(tt)) = (pa, ta) {
                                    self.unify(pt, tt, out);
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // ---- `:=` short declaration / unpack ----

    /// Desugar a collected `:=` (its names plus a resolved `rhs`) into a declaration.
    /// One name becomes an inferred-type `VarDecl`; two or more become a tuple unpack.
    fn build_unpack(
        &mut self,
        mut names: Vec<Option<String>>,
        rhs: Expr,
        span: Span,
    ) -> MResult<StmtKind> {
        if names.len() == 1 {
            let Some(name) = names.pop().unwrap() else {
                return self.err(span.pos, "`_ := e` binds nothing — write `e;` instead");
            };
            let Some(ty) = self.type_expr(&rhs) else {
                return self.err(
                    span.pos,
                    "cannot infer the type for `:=`; the right-hand side's type is not \
                     known — declare it with an explicit type",
                );
            };
            let ty = self.resolve_type(&ty, span.pos)?;
            return Ok(StmtKind::VarDecl {
                decls: vec![Declarator {
                    name,
                    ty,
                    init: Some(rhs),
                    span,
                    is_public: false,
                }],
            });
        }
        let Some(elems) = self.tuple_elem_types(&rhs) else {
            return self.err(
                span.pos,
                "cannot infer the tuple element types for `:=`; the right-hand side's \
                 type is not known — bind it to a typed variable first",
            );
        };
        if elems.len() != names.len() {
            return self.err(
                span.pos,
                format!(
                    "`:=` binds {} name(s) but the tuple has {} element(s)",
                    names.len(),
                    elems.len()
                ),
            );
        }
        let slots: Vec<(Type, Option<String>)> = elems.into_iter().zip(names).collect();
        self.desugar_destructure(slots, rhs, span)
    }

    /// Best-effort tuple element types of an unpack's right-hand side. Handles a tuple
    /// literal `(e0, …)`, typing each element, or any expression whose static type is
    /// a known tuple struct.
    fn tuple_elem_types(&self, rhs: &Expr) -> Option<Vec<Type>> {
        if let ExprKind::InitList(items) = &rhs.kind {
            return items.iter().map(|e| self.type_expr(e)).collect();
        }
        let Type::Named(tup) = self.type_expr(rhs)? else {
            return None;
        };
        if !self.tuple_types.contains(&tup) {
            return None;
        }
        Some(
            self.class_fields
                .get(&tup)?
                .iter()
                .map(|(_, t)| t.clone())
                .collect(),
        )
    }

    /// Lower a tuple unpack into the tuple-struct machinery. Binds `rhs` to a hidden
    /// tuple temp, then declares each named slot from the corresponding field. A `None`
    /// name (`_`) keeps its position but binds nothing.
    fn desugar_destructure(
        &mut self,
        slots: Vec<(Type, Option<String>)>,
        rhs: Expr,
        span: Span,
    ) -> MResult<StmtKind> {
        let elems: Vec<Type> = slots.iter().map(|(t, _)| t.clone()).collect();
        let mut celems = Vec::with_capacity(elems.len());
        for t in &elems {
            celems.push(self.resolve_type(t, span.pos)?);
        }
        let tup = self.intern_tuple(&celems);
        let tmp = format!("$dst{}", span.start);
        let mut decls = vec![Declarator {
            name: tmp.clone(),
            ty: Type::Named(tup),
            init: Some(rhs),
            span,
            is_public: false,
        }];
        for (i, (ty, name)) in celems.iter().zip(slots.iter().map(|(_, n)| n)).enumerate() {
            let Some(name) = name else { continue };
            decls.push(Declarator {
                name: name.clone(),
                ty: ty.clone(),
                init: Some(tuple_field(&tmp, i, span)),
                span,
                is_public: false,
            });
        }
        Ok(StmtKind::VarDecl { decls })
    }
}

/// Build `<var>._<i>`: a read of field `i` of a tuple-typed variable.
fn tuple_field(var: &str, i: usize, span: Span) -> Expr {
    Expr::new(
        ExprKind::Member {
            base: Box::new(Expr::new(ExprKind::Ident(var.to_string()), span)),
            field: format!("_{i}"),
            arrow: false,
        },
        span,
    )
}

// ---- pure parameter substitution (template -> param-free, still deferred) ----

/// Substitute a template's bound parameters in `ty`. A type parameter becomes its
/// bound type; a value parameter referenced in an array dimension becomes its bound
/// `Int`. Leaves the deferred node kinds (`Generic`, `Tuple`) intact for the resolver.
fn subst_type(ty: &Type, binds: &HashMap<String, Bind>) -> Type {
    match ty {
        Type::Param(n) => match binds.get(n) {
            Some(Bind::Type(t)) => t.clone(),
            _ => Type::Param(n.clone()),
        },
        Type::Generic(name, args) => Type::Generic(
            name.clone(),
            args.iter().map(|a| subst_arg(a, binds)).collect(),
        ),
        Type::Tuple(elems) => Type::Tuple(elems.iter().map(|a| subst_type(a, binds)).collect()),
        Type::Ptr(inner) => Type::Ptr(Box::new(subst_type(inner, binds))),
        // Substitute the dimension expression too, so a value-param dim `T data[N]`
        // becomes `T data[8]` (`Ident("N")` → `Int(8)`).
        Type::Array(inner, dim) => Type::Array(
            Box::new(subst_type(inner, binds)),
            dim.as_ref().map(|e| Box::new(subst_expr(e, binds))),
        ),
        Type::FuncPtr { ret, params } => Type::FuncPtr {
            ret: Box::new(subst_type(ret, binds)),
            params: params.iter().map(|p| subst_type(p, binds)).collect(),
        },
        other => other.clone(),
    }
}

/// Substitute in one generic argument: a type arg recurses through [`subst_type`], a
/// value arg through [`subst_expr`] (turning a value-param `Ident` into its `Int`).
fn subst_arg(a: &GenericArg, binds: &HashMap<String, Bind>) -> GenericArg {
    match a {
        GenericArg::Type(t) => GenericArg::Type(subst_type(t, binds)),
        GenericArg::Value(e) => GenericArg::Value(Box::new(subst_expr(e, binds))),
    }
}

fn subst_stmt(s: &Stmt, binds: &HashMap<String, Bind>) -> Stmt {
    let kind = match &s.kind {
        StmtKind::Empty
        | StmtKind::Default
        | StmtKind::SwitchStart
        | StmtKind::SwitchEnd
        | StmtKind::Break
        | StmtKind::Continue
        | StmtKind::Goto(_)
        | StmtKind::Label(_)
        | StmtKind::Include(_) => s.kind.clone(),
        StmtKind::Expr(e) => StmtKind::Expr(subst_expr(e, binds)),
        StmtKind::Block(ss) => StmtKind::Block(ss.iter().map(|s| subst_stmt(s, binds)).collect()),
        StmtKind::ShortDecl { names, rhs } => StmtKind::ShortDecl {
            names: names.clone(),
            rhs: subst_expr(rhs, binds),
        },
        StmtKind::VarDecl { decls } => StmtKind::VarDecl {
            decls: decls
                .iter()
                .map(|d| Declarator {
                    name: d.name.clone(),
                    ty: subst_type(&d.ty, binds),
                    init: d.init.as_ref().map(|e| subst_expr(e, binds)),
                    span: d.span,
                    is_public: d.is_public,
                })
                .collect(),
        },
        StmtKind::If { cond, then, else_ } => StmtKind::If {
            cond: subst_expr(cond, binds),
            then: Box::new(subst_stmt(then, binds)),
            else_: else_.as_ref().map(|s| Box::new(subst_stmt(s, binds))),
        },
        StmtKind::While { cond, body } => StmtKind::While {
            cond: subst_expr(cond, binds),
            body: Box::new(subst_stmt(body, binds)),
        },
        StmtKind::DoWhile { body, cond } => StmtKind::DoWhile {
            body: Box::new(subst_stmt(body, binds)),
            cond: subst_expr(cond, binds),
        },
        StmtKind::For {
            init,
            cond,
            step,
            body,
        } => StmtKind::For {
            init: init.as_ref().map(|s| Box::new(subst_stmt(s, binds))),
            cond: cond.as_ref().map(|e| subst_expr(e, binds)),
            step: step.as_ref().map(|e| subst_expr(e, binds)),
            body: Box::new(subst_stmt(body, binds)),
        },
        StmtKind::Switch { cond, body } => StmtKind::Switch {
            cond: subst_expr(cond, binds),
            body: Box::new(subst_stmt(body, binds)),
        },
        StmtKind::Case { lo, hi } => StmtKind::Case {
            lo: subst_expr(lo, binds),
            hi: hi.as_ref().map(|e| subst_expr(e, binds)),
        },
        StmtKind::Return(e) => StmtKind::Return(e.as_ref().map(|e| subst_expr(e, binds))),
        StmtKind::TypeSwitch { on, arms, default } => StmtKind::TypeSwitch {
            on: match on {
                TypeSwitchOn::Ty(t) => TypeSwitchOn::Ty(subst_type(t, binds)),
                TypeSwitchOn::Val(e) => TypeSwitchOn::Val(Box::new(subst_expr(e, binds))),
            },
            arms: arms
                .iter()
                .map(|(t, body)| {
                    (
                        subst_type(t, binds),
                        body.iter().map(|s| subst_stmt(s, binds)).collect(),
                    )
                })
                .collect(),
            default: default
                .as_ref()
                .map(|body| body.iter().map(|s| subst_stmt(s, binds)).collect()),
        },
        StmtKind::Try { body, handler } => StmtKind::Try {
            body: body.iter().map(|s| subst_stmt(s, binds)).collect(),
            handler: handler.iter().map(|s| subst_stmt(s, binds)).collect(),
        },
        StmtKind::Throw(e) => StmtKind::Throw(e.as_ref().map(|e| subst_expr(e, binds))),
        StmtKind::Func(_) | StmtKind::Class(_) => s.kind.clone(),
    };
    Stmt::new(kind, s.span)
}

fn subst_expr(e: &Expr, binds: &HashMap<String, Bind>) -> Expr {
    let bx = |e: &Expr| Box::new(subst_expr(e, binds));
    let kind = match &e.kind {
        // A value (`int`) parameter referenced in the body becomes its bound literal.
        ExprKind::Ident(n) => match binds.get(n) {
            Some(Bind::Value(v)) => ExprKind::Int(*v),
            _ => e.kind.clone(),
        },
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Str(_)
        | ExprKind::Char(_)
        | ExprKind::Offset { .. } => e.kind.clone(),
        ExprKind::Unary { op, expr } => ExprKind::Unary {
            op: *op,
            expr: bx(expr),
        },
        ExprKind::Postfix { op, expr } => ExprKind::Postfix {
            op: *op,
            expr: bx(expr),
        },
        ExprKind::Binary { op, lhs, rhs } => ExprKind::Binary {
            op: *op,
            lhs: bx(lhs),
            rhs: bx(rhs),
        },
        ExprKind::Assign { op, target, value } => ExprKind::Assign {
            op: *op,
            target: bx(target),
            value: bx(value),
        },
        ExprKind::Ternary { cond, then, else_ } => ExprKind::Ternary {
            cond: bx(cond),
            then: bx(then),
            else_: bx(else_),
        },
        ExprKind::Call { callee, args } => ExprKind::Call {
            callee: bx(callee),
            args: args.iter().map(|a| subst_expr(a, binds)).collect(),
        },
        ExprKind::GenericCall {
            name,
            type_args,
            args,
        } => ExprKind::GenericCall {
            name: name.clone(),
            type_args: type_args.iter().map(|a| subst_arg(a, binds)).collect(),
            args: args.iter().map(|a| subst_expr(a, binds)).collect(),
        },
        ExprKind::Index { base, index } => ExprKind::Index {
            base: bx(base),
            index: bx(index),
        },
        ExprKind::Member { base, field, arrow } => ExprKind::Member {
            base: bx(base),
            field: field.clone(),
            arrow: *arrow,
        },
        ExprKind::Cast { ty, expr } => ExprKind::Cast {
            ty: subst_type(ty, binds),
            expr: bx(expr),
        },
        ExprKind::Sizeof(SizeofArg::Type(t)) => {
            ExprKind::Sizeof(SizeofArg::Type(subst_type(t, binds)))
        }
        ExprKind::Sizeof(SizeofArg::Expr(ex)) => ExprKind::Sizeof(SizeofArg::Expr(bx(ex))),
        ExprKind::InitList(items) => {
            ExprKind::InitList(items.iter().map(|e| subst_expr(e, binds)).collect())
        }
        ExprKind::DesignatedInit(pairs) => ExprKind::DesignatedInit(
            pairs
                .iter()
                .map(|(n, ex)| (n.clone(), subst_expr(ex, binds)))
                .collect(),
        ),
        ExprKind::Comma(items) => {
            ExprKind::Comma(items.iter().map(|e| subst_expr(e, binds)).collect())
        }
    };
    Expr::new(kind, e.span)
}
