//! Monomorphization — the **`mono` pass**. It resolves the generic constructs the
//! parser defers, turning them into ordinary concrete AST, *after* parsing and
//! *before* sema/layout/codegen. The parser emits four deferred node kinds and
//! never instantiates anything itself:
//!
//! * `Type::Generic(name, args)` — a generic-class use (`Vec<I64>`),
//! * `Type::Tuple(elems)` — a tuple type (`(I64, F64)`),
//! * `ExprKind::GenericCall { name, type_args, args }` — a generic-function call
//!   (explicit `Id<I64>(x)` or inferred `Id(x)`),
//! * `StmtKind::ShortDecl { names, rhs }` — a `:=` short declaration / unpack.
//!
//! This pass owns the whole monomorphization worklist *and* a real, scoped,
//! recursive typer ([`Mono::type_expr`]) over the already-parsed program. Because
//! it runs after the full parse it sees forward declarations, ternaries,
//! function-pointer results, and inherited (base-class) fields — closing the gaps
//! of the old parse-time, "seen so far" inference (`arg_type`).
//!
//! Synthetic definitions it generates (instantiated classes/functions, interned
//! tuple structs, unpack temporaries) are stamped with `file: 0` spans — the
//! public program root — so `_`-directory privacy never rejects a monomorphized
//! instance of a public template.

use crate::ast::*;
use crate::parser::{mangle_generic, tuple_type_name};
use crate::token::{Pos, Span};
use std::collections::{HashMap, HashSet};

/// An error raised while monomorphizing (an un-inferable type argument, a generic
/// arity mismatch, a non-inferable `:=`). Mirrors `ParseError`'s shape so the CLIs
/// can report it the same way.
#[derive(Debug, Clone)]
pub struct MonoError {
    pub message: String,
    pub pos: Pos,
}

type MResult<T> = Result<T, MonoError>;

/// Expand every deferred generic construct in `program`, returning a fully concrete
/// program — no `Type::Generic`/`Type::Tuple`/`GenericCall`/`ShortDecl` remain, so
/// sema, layout, the interpreter, and the backends only ever see ordinary AST.
pub fn expand(mut program: Program) -> MResult<Program> {
    let templates = std::mem::take(&mut program.generics);
    let mut m = Mono::new(templates);
    let mut items = std::mem::take(&mut program.items);
    m.run(&mut items)?;
    items.append(&mut m.generated);
    program.items = items;
    Ok(program)
}

/// A synthetic span for generated definitions: the public program root (`file: 0`).
fn syn_span() -> Span {
    Span::dummy()
}

struct Mono {
    /// Generic `class`/`union` templates, by name (parsed once, parameters symbolic).
    classes: HashMap<String, GenericClass>,
    /// Generic function templates, by name.
    fns: HashMap<String, GenericFn>,
    /// Mangled names of class instances already generated (dedup).
    class_done: HashSet<String>,
    /// Mangled names of function instances already generated (dedup).
    fn_done: HashSet<String>,
    /// Class-instance mangled name → `(template, type-args)`, so a `Vec<T>` parameter
    /// can be unified against a `Vec_I64` argument to bind `T`.
    instances: HashMap<String, (String, Vec<Type>)>,
    /// Concrete class/union/tuple field types, by type name, for the typer.
    class_fields: HashMap<String, Vec<(String, Type)>>,
    /// Each concrete class/union's base (for inherited-field lookup in the typer).
    class_bases: HashMap<String, Option<String>>,
    /// Concrete function/instance return types, for the typer.
    fn_rets: HashMap<String, Type>,
    /// Canonical names of tuple structs already interned (dedup).
    tuple_types: HashSet<String>,
    /// Pending class instantiations `(template, type-args)`.
    pending_classes: Vec<(String, Vec<Type>)>,
    /// Pending function instantiations `(template, type-args)`.
    pending_fns: Vec<(String, Vec<Type>)>,
    /// Generated concrete definitions (instances + interned tuples), appended to the
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
        // Pass 1: resolve all top-level signatures (function ret/params, class fields)
        // and record them, so forward references resolve during body resolution.
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

    /// Resolve a top-level item's body / executable part. Functions get their own
    /// scope (parameters); everything else is an ordinary top-level statement.
    fn resolve_top(&mut self, s: &mut Stmt) -> MResult<()> {
        match &mut s.kind {
            StmtKind::Func(f) => {
                self.scopes.push(HashMap::new());
                for p in &f.params {
                    if let Some(pname) = &p.name {
                        self.scope_insert(pname.clone(), p.ty.clone());
                    }
                }
                // Resolve any parameter default expressions (in the function scope).
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
            StmtKind::Class(_) => Ok(()), // fields already resolved in the sig pass
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
            // A nested function/class definition isn't supported (sema and the backends
            // reject it), but `mono` still resolves it so the rejection is theirs to
            // make — it isn't `mono`'s place to gate it.
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
                // Resolve the explicit type arguments and the value arguments first.
                let mut targs = Vec::with_capacity(type_args.len());
                for t in &type_args {
                    targs.push(self.resolve_type(t, pos)?);
                }
                for a in args.iter_mut() {
                    self.resolve_expr(a)?;
                }
                // Infer the type arguments from the (resolved) argument types if none
                // were given.
                let targs = if targs.is_empty() {
                    self.infer(&name, &args, pos)?
                } else {
                    targs
                };
                let mangled = mangle_generic(&name, &targs);
                self.record_instance_ret(&name, &targs, &mangled, pos)?;
                self.pending_fns.push((name, targs));
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

    /// Resolve a type: a `Type::Generic` instantiates its class (queuing it) and
    /// becomes the mangled `Named`; a `Type::Tuple` interns its struct; pointer/array/
    /// function-pointer wrappers recurse.
    fn resolve_type(&mut self, ty: &Type, pos: Pos) -> MResult<Type> {
        Ok(match ty {
            Type::Generic(name, args) => {
                let cargs = args
                    .iter()
                    .map(|a| self.resolve_type(a, pos))
                    .collect::<MResult<Vec<_>>>()?;
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
                Type::Array(Box::new(self.resolve_type(inner, pos)?), dim.clone())
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

    /// Record a class instantiation request (deduped), returning its mangled `Named`
    /// type. The concrete definition is generated later, in `drain`.
    fn instantiate_class_ref(&mut self, name: &str, args: Vec<Type>, pos: Pos) -> MResult<Type> {
        let nparams = self
            .classes
            .get(name)
            .map(|t| t.params.len())
            .ok_or_else(|| MonoError {
                message: format!("`{name}` is not a generic type"),
                pos,
            })?;
        if args.len() != nparams {
            return self.err(
                pos,
                format!(
                    "generic `{name}` expects {nparams} type argument(s), got {}",
                    args.len()
                ),
            );
        }
        let mangled = mangle_generic(name, &args);
        self.instances
            .insert(mangled.clone(), (name.to_string(), args.clone()));
        if self.class_done.insert(mangled.clone()) {
            self.pending_classes.push((name.to_string(), args));
        }
        Ok(Type::Named(mangled))
    }

    /// Intern the canonical tuple struct for `elems`, generating the synthetic
    /// `class $Tup$…` once, and return its type name.
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
                })
                .collect();
            self.generated.push(Stmt::new(
                StmtKind::Class(ClassDef {
                    is_union: false,
                    name: name.clone(),
                    base: None,
                    fields,
                }),
                syn_span(),
            ));
        }
        name
    }

    // ---- instantiation (parameter substitution + resolution) ----

    /// Drain the instantiation worklists to a fixpoint. Generating one instance may
    /// request more of either (a body's nested generic use/call), so loop until both
    /// queues empty.
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

    fn instantiate_class(&mut self, name: &str, args: &[Type]) -> MResult<Stmt> {
        let tmpl = self.classes.get(name).expect("generic class").clone();
        let binds: HashMap<String, Type> = tmpl
            .params
            .iter()
            .cloned()
            .zip(args.iter().cloned())
            .collect();
        let mangled = mangle_generic(name, args);
        let mut fields = Vec::with_capacity(tmpl.fields.len());
        for f in &tmpl.fields {
            fields.push(Declarator {
                name: f.name.clone(),
                ty: subst_type(&f.ty, &binds),
                init: None,
                span: f.span,
            });
        }
        let mut stmt = Stmt::new(
            StmtKind::Class(ClassDef {
                is_union: tmpl.is_union,
                name: mangled,
                base: tmpl.base.clone(),
                fields,
            }),
            syn_span(),
        );
        self.resolve_sig(&mut stmt)?; // resolve the substituted field types + record
        Ok(stmt)
    }

    fn instantiate_fn(&mut self, name: &str, args: &[Type], mangled: &str) -> MResult<Stmt> {
        let tmpl = self.fns.get(name).expect("generic fn").clone();
        if args.len() != tmpl.type_params.len() {
            return self.err(
                Pos::new(0, 0),
                format!(
                    "generic function `{name}` expects {} type argument(s), got {}",
                    tmpl.type_params.len(),
                    args.len()
                ),
            );
        }
        let binds: HashMap<String, Type> = tmpl
            .type_params
            .iter()
            .cloned()
            .zip(args.iter().cloned())
            .collect();
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
            }),
            syn_span(),
        );
        self.resolve_sig(&mut stmt)?;
        self.resolve_top(&mut stmt)?;
        Ok(stmt)
    }

    /// Record a generic-function instance's concrete return type, so a call site
    /// (and `:=`) can see through it. Substitutes the bound type arguments into the
    /// template's return type, then resolves it.
    fn record_instance_ret(
        &mut self,
        name: &str,
        args: &[Type],
        mangled: &str,
        pos: Pos,
    ) -> MResult<()> {
        let tmpl = self.fns.get(name).cloned().ok_or_else(|| MonoError {
            message: format!("`{name}` is not a generic function"),
            pos,
        })?;
        if args.len() != tmpl.type_params.len() {
            return self.err(
                pos,
                format!(
                    "generic function `{name}` expects {} type argument(s), got {}",
                    tmpl.type_params.len(),
                    args.len()
                ),
            );
        }
        let binds: HashMap<String, Type> = tmpl
            .type_params
            .iter()
            .cloned()
            .zip(args.iter().cloned())
            .collect();
        let ret = self.resolve_type(&subst_type(&tmpl.def.ret, &binds), pos)?;
        self.fn_rets.insert(mangled.to_string(), ret);
        Ok(())
    }

    // ---- call-site type-argument inference ----

    /// Infer a generic function's type arguments from its (resolved) argument
    /// expressions, by unifying each parameter's template type against the argument's
    /// static type.
    fn infer(&self, name: &str, args: &[Expr], pos: Pos) -> MResult<Vec<Type>> {
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
        let mut type_args = Vec::with_capacity(tmpl.type_params.len());
        for tp in &tmpl.type_params {
            match binds.get(tp) {
                Some(t) => type_args.push(t.clone()),
                None => {
                    return self.err(
                        pos,
                        format!(
                            "cannot infer type argument `{tp}` for generic `{name}`; \
                             call it as `{name}<...>(...)`"
                        ),
                    );
                }
            }
        }
        Ok(type_args)
    }

    /// The static type of an expression — a complete, scoped, recursive typer over
    /// the resolved program. `None` when a type can't be determined (then a generic
    /// call must give explicit type arguments). Used for type-argument inference and
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
            ExprKind::Binary { op, lhs, rhs } => match op {
                BinOp::Eq
                | BinOp::Ne
                | BinOp::Lt
                | BinOp::Gt
                | BinOp::Le
                | BinOp::Ge
                | BinOp::And
                | BinOp::Or => Some(Type::I64),
                _ => {
                    let f = matches!(self.type_expr(lhs), Some(Type::F64))
                        || matches!(self.type_expr(rhs), Some(Type::F64));
                    Some(if f { Type::F64 } else { Type::I64 })
                }
            },
            // A ternary's type is its arms' common type (prefer the `then` arm).
            ExprKind::Ternary { then, else_, .. } => {
                self.type_expr(then).or_else(|| self.type_expr(else_))
            }
            ExprKind::Sizeof(_) => Some(Type::I64),
            _ => None,
        }
    }

    /// The return type of a call whose callee is `callee`: a named function (or
    /// generic instance), or a function-pointer value (a variable or a class field).
    fn call_ret(&self, callee: &Expr) -> Option<Type> {
        match &callee.kind {
            ExprKind::Ident(n) => {
                if let Some(ret) = self.fn_rets.get(n) {
                    return Some(ret.clone());
                }
                // A function-pointer variable.
                match self.lookup_var(n)? {
                    Type::FuncPtr { ret, .. } => Some(*ret),
                    _ => None,
                }
            }
            // A function-pointer field/result: type the callee, expect a `FuncPtr`.
            _ => match self.type_expr(callee)? {
                Type::FuncPtr { ret, .. } => Some(*ret),
                _ => None,
            },
        }
    }

    /// The class/union name a value's type names (a `Named`, or a pointer to one).
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

    /// A field's type in class `cname`, walking the base-class chain for an inherited
    /// field.
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
    /// binding type parameters. A `Generic` parameter (`Vec<T>`) is matched against a
    /// concrete instance argument (`Vec_I64`) via the instance map.
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
                            for (pa, ta) in gargs.iter().zip(iargs.iter()) {
                                self.unify(pa, ta, out);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // ---- `:=` short declaration / unpack ----

    /// Desugar a collected `:=` (names + a resolved `rhs`) into a declaration: one
    /// name → an inferred-type `VarDecl`; two or more → a tuple unpack.
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

    /// Best-effort tuple element types of an unpack's right-hand side: a tuple literal
    /// `(e0, …)` (each element typed), or any expression whose static type is a known
    /// tuple struct.
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

    /// Lower a tuple unpack into the tuple-struct machinery: bind `rhs` to a hidden
    /// tuple temp, then declare each named slot from its field. A `None` name (`_`)
    /// keeps its place but binds nothing.
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
        }];
        for (i, (ty, name)) in celems.iter().zip(slots.iter().map(|(_, n)| n)).enumerate() {
            let Some(name) = name else { continue };
            decls.push(Declarator {
                name: name.clone(),
                ty: ty.clone(),
                init: Some(tuple_field(&tmp, i, span)),
                span,
            });
        }
        Ok(StmtKind::VarDecl { decls })
    }
}

/// `<var>._<i>` — read field `i` of a tuple-typed variable.
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

// ---- pure parameter substitution (template → param-free, still deferred) ----

/// Substitute a template's bound type parameters in `ty`, leaving the deferred node
/// *kinds* (`Generic`/`Tuple`) intact for the resolver to handle.
fn subst_type(ty: &Type, binds: &HashMap<String, Type>) -> Type {
    match ty {
        Type::Param(n) => binds
            .get(n)
            .cloned()
            .unwrap_or_else(|| Type::Param(n.clone())),
        Type::Generic(name, args) => Type::Generic(
            name.clone(),
            args.iter().map(|a| subst_type(a, binds)).collect(),
        ),
        Type::Tuple(elems) => Type::Tuple(elems.iter().map(|a| subst_type(a, binds)).collect()),
        Type::Ptr(inner) => Type::Ptr(Box::new(subst_type(inner, binds))),
        Type::Array(inner, dim) => Type::Array(Box::new(subst_type(inner, binds)), dim.clone()),
        Type::FuncPtr { ret, params } => Type::FuncPtr {
            ret: Box::new(subst_type(ret, binds)),
            params: params.iter().map(|p| subst_type(p, binds)).collect(),
        },
        other => other.clone(),
    }
}

fn subst_stmt(s: &Stmt, binds: &HashMap<String, Type>) -> Stmt {
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
        StmtKind::Func(_) | StmtKind::Class(_) => s.kind.clone(),
    };
    Stmt::new(kind, s.span)
}

fn subst_expr(e: &Expr, binds: &HashMap<String, Type>) -> Expr {
    let bx = |e: &Expr| Box::new(subst_expr(e, binds));
    let kind = match &e.kind {
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Str(_)
        | ExprKind::Char(_)
        | ExprKind::Ident(_)
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
            type_args: type_args.iter().map(|t| subst_type(t, binds)).collect(),
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
