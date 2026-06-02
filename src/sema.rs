//! Semantic analysis for HolyC: name resolution, type inference, and a set of
//! validity checks over the parsed [`Program`].
//!
//! HolyC is a weakly-typed, C-like language: the default integer is `I64`,
//! pointers and integers convert freely, and comparison/logical results are
//! `I64`. The analyzer reflects that — it is permissive about scalar
//! conversions and focuses on catching genuine mistakes:
//!
//!   * use of undeclared variables, unknown types, and unknown fields,
//!   * redeclaration of variables, parameters, fields, functions, and types,
//!   * `break`/`continue`/`case`/`default` used out of context,
//!   * `goto` to a label that does not exist in the function,
//!   * `return` that disagrees with the function's return type,
//!   * non-scalar conditions, indexing non-pointers, member access on
//!     non-aggregates, assigning to non-lvalues, and `&` of non-lvalues.
//!
//! Errors are collected (analysis does not stop at the first one) and each
//! carries a source position.
//!
//! Scope note: a call to a name that is neither a user function nor a known
//! intrinsic (see `seed_builtin_funcs`) is a compile-time error. The core and
//! standard libraries are not modelled yet; their functions will be registered
//! as intrinsics when they land.

use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::token::Pos;

/// A semantic error with the source position where it was detected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemaError {
    pub message: String,
    pub pos: Pos,
}

impl std::fmt::Display for SemaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "semantic error at {}: {}", self.pos, self.message)
    }
}

/// Type-check and resolve names in `program`. Returns all errors found, or an
/// empty `Vec` if the program is well-formed.
pub fn check_program(program: &Program) -> Vec<SemaError> {
    let mut a = Analyzer::new();
    a.run(program);
    a.errors
}

/// Convenience wrapper returning a `Result`.
pub fn analyze(program: &Program) -> Result<(), Vec<SemaError>> {
    let errs = check_program(program);
    if errs.is_empty() { Ok(()) } else { Err(errs) }
}

/// A class/union definition, flattened for field lookup. Each field keeps its
/// source position so type-reference errors can point at it.
struct TypeDef {
    fields: Vec<(String, Type, Pos)>,
    base: Option<String>,
    /// Position of the definition, used to locate base-class errors.
    base_pos: Pos,
}

/// A function signature.
struct FuncSig {
    ret: Type,
    /// Declared parameter types (for building `&Func` function-pointer types).
    params: Vec<Type>,
    /// Parameter count that must be supplied (those without a default).
    required: usize,
    /// Total declared parameter count.
    total: usize,
    varargs: bool,
    /// Whether a definition (not just a prototype) has been seen.
    defined: bool,
}

struct Analyzer {
    errors: Vec<SemaError>,
    types: HashMap<String, TypeDef>,
    funcs: HashMap<String, FuncSig>,
    /// Lexical variable scopes; `scopes[0]` is the global scope.
    scopes: Vec<HashMap<String, Type>>,
    /// Return type of the function currently being checked, if any.
    cur_ret: Option<Type>,
    loop_depth: u32,
    switch_depth: u32,
    /// Stack of label scopes: the labels declared directly in the current block
    /// and each enclosing block. A `goto` is valid iff its target is in one of
    /// these — the same reachability rule the interpreter resolves gotos by.
    label_scopes: Vec<HashSet<String>>,
}

impl Analyzer {
    fn new() -> Self {
        Analyzer {
            errors: Vec::new(),
            types: HashMap::new(),
            funcs: HashMap::new(),
            scopes: Vec::new(),
            cur_ret: None,
            loop_depth: 0,
            switch_depth: 0,
            label_scopes: Vec::new(),
        }
    }

    fn run(&mut self, program: &Program) {
        self.scopes.push(HashMap::new()); // global scope
        self.seed_builtins();
        self.collect_types(program);
        self.collect_funcs(program);
        self.seed_builtin_funcs();
        self.validate_type_refs();
        self.check_layouts(program);
        // Top-level statements form an implicit body with their own label scope,
        // so `goto`/labels work at the top level too.
        self.label_scopes.push(direct_labels(&program.items));
        for item in &program.items {
            self.check_top_item(item);
        }
        self.label_scopes.pop();
        self.scopes.pop();
    }

    /// HolyC predefines a few constants that any program may reference.
    fn seed_builtins(&mut self) {
        for name in ["NULL", "TRUE", "FALSE"] {
            self.scopes[0].insert(name.to_string(), Type::I64);
        }
    }

    /// Register the intrinsic functions (from the shared `builtins` registry) so
    /// calls to them aren't flagged as undeclared. User definitions of the same
    /// name win (hence `or_insert`).
    fn seed_builtin_funcs(&mut self) {
        for b in crate::builtins::all() {
            let n = b.params.len();
            self.funcs.entry(b.name.to_string()).or_insert(FuncSig {
                ret: b.ret,
                params: b.params,
                required: n,
                total: n,
                varargs: b.varargs,
                defined: true,
            });
        }
    }

    fn error(&mut self, pos: Pos, msg: impl Into<String>) {
        self.errors.push(SemaError {
            message: msg.into(),
            pos,
        });
    }

    // ---- collection passes ----

    fn collect_types(&mut self, program: &Program) {
        for item in &program.items {
            if let StmtKind::Class(c) = &item.kind {
                if self.types.contains_key(&c.name) {
                    self.error(item.span.pos, format!("redefinition of type `{}`", c.name));
                    continue;
                }
                let fields = c
                    .fields
                    .iter()
                    .map(|d| (d.name.clone(), d.ty.clone(), d.span.pos))
                    .collect();
                self.types.insert(
                    c.name.clone(),
                    TypeDef {
                        fields,
                        base: c.base.clone(),
                        base_pos: item.span.pos,
                    },
                );
            }
        }
    }

    fn collect_funcs(&mut self, program: &Program) {
        for item in &program.items {
            if let StmtKind::Func(f) = &item.kind {
                let required = f.params.iter().filter(|p| p.default.is_none()).count();
                let has_body = f.body.is_some();
                if let Some(existing) = self.funcs.get(&f.name) {
                    if existing.defined && has_body {
                        self.error(
                            item.span.pos,
                            format!("redefinition of function `{}`", f.name),
                        );
                        continue;
                    }
                    // A prototype followed by a definition (or vice versa) is
                    // fine; keep the entry, marking it defined if either is.
                    let defined = existing.defined || has_body;
                    self.funcs.get_mut(&f.name).unwrap().defined = defined;
                } else {
                    self.funcs.insert(
                        f.name.clone(),
                        FuncSig {
                            ret: f.ret.clone(),
                            params: f.params.iter().map(|p| p.ty.clone()).collect(),
                            required,
                            total: f.params.len(),
                            varargs: f.varargs,
                            defined: has_body,
                        },
                    );
                }
            }
        }
    }

    /// Run the layout pass and surface its errors (cyclic by-value types,
    /// non-constant field array sizes) as semantic errors.
    fn check_layouts(&mut self, program: &Program) {
        let (_, errs) = crate::layout::compute(program);
        for e in errs {
            self.error(e.pos, e.message);
        }
    }

    /// After all types are registered, confirm that field and base-class type
    /// references actually exist.
    fn validate_type_refs(&mut self) {
        // Collect the work first to avoid borrowing `self.types` while pushing
        // errors.
        let mut refs: Vec<(Type, Pos)> = Vec::new();
        let mut base_refs: Vec<(String, Pos)> = Vec::new();
        let names: Vec<String> = self.types.keys().cloned().collect();
        for name in &names {
            let def = &self.types[name];
            for (_, ty, pos) in &def.fields {
                refs.push((ty.clone(), *pos));
            }
            if let Some(b) = &def.base {
                base_refs.push((b.clone(), def.base_pos));
            }
        }
        for (ty, pos) in refs {
            self.resolve_type(&ty, pos);
        }
        for (b, pos) in base_refs {
            if !self.types.contains_key(&b) {
                self.error(pos, format!("unknown base type `{b}`"));
            }
        }
    }

    // ---- scope helpers ----

    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    /// Declare a variable in the current scope, reporting a redeclaration if the
    /// name already exists at this level.
    fn declare(&mut self, name: &str, ty: Type, pos: Pos) {
        let scope = self.scopes.last_mut().unwrap();
        if scope.contains_key(name) {
            self.error(pos, format!("redeclaration of `{name}`"));
        } else {
            scope.insert(name.to_string(), ty);
        }
    }

    fn lookup_var(&self, name: &str) -> Option<&Type> {
        self.scopes.iter().rev().find_map(|s| s.get(name))
    }

    // ---- type resolution & predicates ----

    /// Confirm a type's named parts exist; report and continue otherwise.
    fn resolve_type(&mut self, ty: &Type, pos: Pos) {
        match ty {
            Type::Named(n) => {
                if !self.types.contains_key(n) {
                    self.error(pos, format!("unknown type `{n}`"));
                }
            }
            Type::Ptr(inner) => self.resolve_type(inner, pos),
            Type::Array(inner, dim) => {
                self.resolve_type(inner, pos);
                if let Some(d) = dim {
                    let dt = self.check_expr(d);
                    if !is_integer(&dt) {
                        self.error(d.span.pos, "array size must be an integer");
                    }
                }
            }
            _ => {}
        }
    }

    /// An array decays to a pointer in value contexts.
    fn decay(ty: Type) -> Type {
        match ty {
            Type::Array(inner, _) => Type::Ptr(inner),
            other => other,
        }
    }

    // ---- top-level & statements ----

    fn check_top_item(&mut self, item: &Stmt) {
        match &item.kind {
            StmtKind::Func(f) => self.check_function(f),
            StmtKind::Class(_) | StmtKind::Include(_) => {}
            _ => self.check_stmt(item),
        }
    }

    fn check_function(&mut self, f: &FuncDef) {
        let Some(body) = &f.body else {
            return; // prototype: nothing to check
        };
        self.resolve_type(&f.ret, Pos::new(0, 0));
        self.cur_ret = Some(f.ret.clone());

        self.label_scopes.push(direct_labels(body));
        self.push_scope();
        for p in &f.params {
            self.resolve_type(&p.ty, p.span.pos);
            if matches!(p.ty, Type::U0) {
                self.error(p.span.pos, "parameter cannot have type U0");
            }
            if let Some(d) = &p.default {
                self.check_expr(d);
            }
            if let Some(name) = &p.name {
                self.declare(name, Self::decay(p.ty.clone()), p.span.pos);
            }
        }
        for stmt in body {
            self.check_stmt(stmt);
        }
        self.pop_scope();
        self.label_scopes.pop();
        self.cur_ret = None;
    }

    fn check_stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::Empty | StmtKind::Label(_) | StmtKind::Include(_) => {}
            StmtKind::Expr(e) => {
                self.check_expr(e);
            }
            StmtKind::Block(stmts) => {
                self.label_scopes.push(direct_labels(stmts));
                self.push_scope();
                for s in stmts {
                    self.check_stmt(s);
                }
                self.pop_scope();
                self.label_scopes.pop();
            }
            StmtKind::VarDecl { decls } => {
                for d in decls {
                    self.check_declarator(d);
                }
            }
            StmtKind::If { cond, then, else_ } => {
                self.check_cond(cond);
                self.check_stmt(then);
                if let Some(e) = else_ {
                    self.check_stmt(e);
                }
            }
            StmtKind::While { cond, body } => {
                self.check_cond(cond);
                self.loop_depth += 1;
                self.check_stmt(body);
                self.loop_depth -= 1;
            }
            StmtKind::DoWhile { body, cond } => {
                self.loop_depth += 1;
                self.check_stmt(body);
                self.loop_depth -= 1;
                self.check_cond(cond);
            }
            StmtKind::For {
                init,
                cond,
                step,
                body,
            } => {
                self.push_scope();
                if let Some(i) = init {
                    self.check_stmt(i);
                }
                if let Some(c) = cond {
                    self.check_cond(c);
                }
                if let Some(s) = step {
                    self.check_expr(s);
                }
                self.loop_depth += 1;
                self.check_stmt(body);
                self.loop_depth -= 1;
                self.pop_scope();
            }
            StmtKind::Switch { cond, body } => {
                let t = self.check_expr(cond);
                if !is_integer(&t) {
                    self.error(cond.span.pos, "switch value must be an integer");
                }
                self.validate_switch_labels(body);
                self.switch_depth += 1;
                self.check_stmt(body);
                self.switch_depth -= 1;
            }
            StmtKind::Case { lo, hi } => {
                if self.switch_depth == 0 {
                    self.error(stmt.span.pos, "`case` outside of a switch");
                }
                self.check_expr(lo);
                if let Some(h) = hi {
                    self.check_expr(h);
                }
            }
            StmtKind::Default => {
                if self.switch_depth == 0 {
                    self.error(stmt.span.pos, "`default` outside of a switch");
                }
            }
            StmtKind::SwitchStart => {
                if self.switch_depth == 0 {
                    self.error(stmt.span.pos, "`start` outside of a switch");
                }
            }
            StmtKind::SwitchEnd => {
                if self.switch_depth == 0 {
                    self.error(stmt.span.pos, "`end` outside of a switch");
                }
            }
            StmtKind::Break => {
                if self.loop_depth == 0 && self.switch_depth == 0 {
                    self.error(stmt.span.pos, "`break` outside of a loop or switch");
                }
            }
            StmtKind::Continue => {
                if self.loop_depth == 0 {
                    self.error(stmt.span.pos, "`continue` outside of a loop");
                }
            }
            StmtKind::Return(val) => self.check_return(val.as_ref(), stmt.span.pos),
            StmtKind::Goto(name) => {
                if !self.label_scopes.iter().any(|s| s.contains(name)) {
                    self.error(
                        stmt.span.pos,
                        format!("goto to undefined or out-of-scope label `{name}`"),
                    );
                }
            }
            // Nested functions/classes are uncommon in HolyC; check a nested
            // function body best-effort and ignore nested class registration.
            StmtKind::Func(f) => self.check_function(f),
            StmtKind::Class(_) => {}
        }
    }

    fn check_declarator(&mut self, d: &Declarator) {
        self.resolve_type(&d.ty, d.span.pos);
        if matches!(d.ty, Type::U0) {
            self.error(
                d.span.pos,
                format!("variable `{}` cannot have type U0", d.name),
            );
        }
        if let Some(init) = &d.init {
            self.check_init(init, &d.ty, init.span.pos);
        }
        self.declare(&d.name, d.ty.clone(), d.span.pos);
    }

    /// Check an initialiser against its declared type. A brace `InitList` is
    /// matched element-by-element against an array's element type or a class's
    /// fields (in layout order); any other expression is checked for
    /// assignability, exactly as a plain `=` initialiser.
    fn check_init(&mut self, init: &Expr, expected: &Type, pos: Pos) {
        if let ExprKind::DesignatedInit(items) = &init.kind {
            self.check_designated_init(init, items, expected, pos);
            return;
        }
        let ExprKind::InitList(items) = &init.kind else {
            let it = self.check_expr(init);
            self.check_assignable(expected, &it, pos);
            return;
        };
        init.set_ty(expected.clone());
        match expected {
            Type::Array(elem, dim) => {
                if let Some(d) = dim {
                    if let ExprKind::Int(n) = &d.kind {
                        if items.len() as i64 > *n {
                            self.error(
                                pos,
                                format!(
                                    "too many initializers ({}) for an array of {n}",
                                    items.len()
                                ),
                            );
                        }
                    }
                }
                for it in items {
                    self.check_init(it, elem, pos);
                }
            }
            Type::Named(class) => {
                let fields = self.class_field_types(class);
                if items.len() > fields.len() {
                    self.error(
                        pos,
                        format!(
                            "too many initializers ({}) for `{class}` ({} fields)",
                            items.len(),
                            fields.len()
                        ),
                    );
                }
                for (it, fty) in items.iter().zip(fields.iter()) {
                    self.check_init(it, fty, pos);
                }
            }
            _ => {
                self.error(
                    pos,
                    "an initializer list can only initialize an array, class, or union",
                );
                for it in items {
                    self.check_expr(it);
                }
            }
        }
    }

    /// Check a designated initialiser `{.field = value, ...}` against its
    /// declared type. The target must be a class; each designator must name an
    /// existing field, and its value is checked against that field's type.
    fn check_designated_init(
        &mut self,
        init: &Expr,
        items: &[(String, Expr)],
        expected: &Type,
        pos: Pos,
    ) {
        init.set_ty(expected.clone());
        let Type::Named(class) = expected else {
            self.error(
                pos,
                "a designated initializer can only initialize a class or union",
            );
            for (_, value) in items {
                self.check_expr(value);
            }
            return;
        };
        let fields = self.class_fields(class);
        for (name, value) in items {
            match fields.iter().find(|(n, _)| n == name) {
                Some((_, fty)) => self.check_init(value, fty, value.span.pos),
                None => {
                    self.error(value.span.pos, format!("`{class}` has no field `{name}`"));
                    self.check_expr(value);
                }
            }
        }
    }

    /// A class/union's `(name, type)` fields in layout order (inherited first).
    fn class_fields(&self, class: &str) -> Vec<(String, Type)> {
        let mut out = Vec::new();
        if let Some(def) = self.types.get(class) {
            if let Some(base) = &def.base {
                out.extend(self.class_fields(base));
            }
            out.extend(
                def.fields
                    .iter()
                    .map(|(name, ty, _)| (name.clone(), ty.clone())),
            );
        }
        out
    }

    /// The function-pointer type of a named user function (for `&Func`), or
    /// `None` if `name` is a builtin or not a function. Builtins are excluded
    /// because they have no address the backends can take.
    fn func_ptr_type(&self, name: &str) -> Option<Type> {
        if crate::builtins::is_builtin(name) {
            return None;
        }
        let sig = self.funcs.get(name)?;
        Some(Type::FuncPtr {
            ret: Box::new(sig.ret.clone()),
            params: sig.params.clone(),
        })
    }

    /// A class/union's field types in layout order: inherited (base) fields
    /// first, then the class's own.
    fn class_field_types(&self, class: &str) -> Vec<Type> {
        let mut out = Vec::new();
        if let Some(def) = self.types.get(class) {
            if let Some(base) = &def.base {
                out.extend(self.class_field_types(base));
            }
            out.extend(def.fields.iter().map(|(_, ty, _)| ty.clone()));
        }
        out
    }

    fn check_cond(&mut self, cond: &Expr) {
        let t = self.check_expr(cond);
        if !is_scalar(&t) {
            self.error(cond.span.pos, "condition must be a scalar value");
        }
    }

    /// Enforce the placement rules for `start:` / `end:` switch sub-labels: at
    /// most one of each, `start:` before every case, `end:` after every case.
    /// This keeps the prologue/epilogue partition unambiguous for both backends.
    fn validate_switch_labels(&mut self, body: &Stmt) {
        let StmtKind::Block(stmts) = &body.kind else {
            return;
        };
        let is_case = |s: &Stmt| matches!(s.kind, StmtKind::Case { .. } | StmtKind::Default);
        let first_case = stmts.iter().position(is_case);
        let last_case = stmts.iter().rposition(is_case);

        let mut start_pos = None;
        let mut end_pos = None;
        for (i, s) in stmts.iter().enumerate() {
            match &s.kind {
                StmtKind::SwitchStart if start_pos.replace(i).is_some() => {
                    self.error(s.span.pos, "duplicate `start:` in a switch");
                }
                StmtKind::SwitchEnd if end_pos.replace(i).is_some() => {
                    self.error(s.span.pos, "duplicate `end:` in a switch");
                }
                _ => {}
            }
        }
        if let (Some(si), Some(fc)) = (start_pos, first_case)
            && si > fc
        {
            self.error(stmts[si].span.pos, "`start:` must come before every `case`");
        }
        if let (Some(ei), Some(lc)) = (end_pos, last_case)
            && ei < lc
        {
            self.error(stmts[ei].span.pos, "`end:` must come after every `case`");
        }
    }

    fn check_return(&mut self, val: Option<&Expr>, pos: Pos) {
        let ret = self.cur_ret.clone();
        match (ret, val) {
            (Some(Type::U0), Some(e)) => {
                self.check_expr(e);
                self.error(pos, "returning a value from a U0 (void) function");
            }
            (Some(rt), Some(e)) if rt != Type::U0 => {
                let vt = self.check_expr(e);
                self.check_assignable(&rt, &vt, e.span.pos);
            }
            (Some(rt), None) if rt != Type::U0 => {
                self.error(pos, "missing return value in non-void function");
            }
            // U0 with no value, or top-level return: fine.
            (_, Some(e)) => {
                self.check_expr(e);
            }
            _ => {}
        }
    }

    // ---- expressions: returns the inferred type ----

    /// Infer an expression's type and record it on the node (building the
    /// typed AST that backends and `sizeof(expr)` rely on).
    fn check_expr(&mut self, expr: &Expr) -> Type {
        let t = self.infer(expr);
        expr.set_ty(t.clone());
        t
    }

    fn infer(&mut self, expr: &Expr) -> Type {
        match &expr.kind {
            ExprKind::Int(_) | ExprKind::Char(_) => Type::I64,
            ExprKind::Float(_) => Type::F64,
            // String literals are U8*.
            ExprKind::Str(_) => Type::Ptr(Box::new(Type::U8)),
            ExprKind::Ident(name) => self.check_ident(name, expr.span.pos),
            ExprKind::Unary { op, expr: inner } => self.check_unary(*op, inner),
            ExprKind::Postfix { op: _, expr: inner } => {
                let t = self.check_expr(inner);
                if !self.is_lvalue(inner) {
                    self.error(inner.span.pos, "operand of `++`/`--` must be an lvalue");
                }
                t
            }
            ExprKind::Binary { op, lhs, rhs } => self.check_binary(*op, lhs, rhs),
            ExprKind::Assign { op, target, value } => self.check_assign(*op, target, value),
            ExprKind::Ternary { cond, then, else_ } => {
                self.check_cond(cond);
                let a = self.check_expr(then);
                let b = self.check_expr(else_);
                arith_result(&a, &b)
            }
            ExprKind::Call { callee, args } => self.check_call(callee, args),
            ExprKind::Index { base, index } => self.check_index(base, index),
            ExprKind::Member { base, field, arrow } => {
                self.check_member(base, field, *arrow, expr.span.pos)
            }
            ExprKind::Cast { ty, expr: inner } => {
                self.resolve_type(ty, inner.span.pos);
                self.check_expr(inner);
                ty.clone()
            }
            ExprKind::Sizeof(arg) => {
                match arg {
                    SizeofArg::Type(t) => self.resolve_type(t, expr.span.pos),
                    // The operand is type-checked so its static type is inferred
                    // and recorded; the size is read from that type later.
                    SizeofArg::Expr(e) => {
                        self.check_expr(e);
                    }
                }
                Type::U64
            }
            ExprKind::Offset { class, path } => {
                self.check_offset(class, path, expr.span.pos);
                Type::I64
            }
            ExprKind::InitList(items) => {
                // A bare initializer list with no target type isn't valid; it is
                // only meaningful in a declarator (handled by `check_init`).
                for it in items {
                    self.check_expr(it);
                }
                self.error(
                    expr.span.pos,
                    "an initializer list is only valid as a variable initializer",
                );
                Type::I64
            }
            ExprKind::DesignatedInit(items) => {
                for (_, value) in items {
                    self.check_expr(value);
                }
                self.error(
                    expr.span.pos,
                    "a designated initializer is only valid as a variable initializer",
                );
                Type::I64
            }
            ExprKind::Comma(items) => {
                let mut last = Type::I64;
                for it in items {
                    last = self.check_expr(it);
                }
                last
            }
        }
    }

    fn check_ident(&mut self, name: &str, pos: Pos) -> Type {
        if let Some(t) = self.lookup_var(name) {
            // Return the variable's true (undecayed) type so the typed AST is
            // accurate — e.g. `sizeof(arr)` sees the array, not a pointer. Use
            // sites (arithmetic, indexing, assignment) apply array-to-pointer
            // decay themselves.
            return t.clone();
        }
        // A bare function name acts like a call in HolyC; give it the return
        // type.
        if let Some(sig) = self.funcs.get(name) {
            return sig.ret.clone();
        }
        self.error(pos, format!("use of undeclared identifier `{name}`"));
        Type::I64
    }

    fn check_unary(&mut self, op: UnOp, inner: &Expr) -> Type {
        let t = self.check_expr(inner);
        match op {
            UnOp::Neg | UnOp::Pos => {
                if !is_arithmetic(&t) {
                    self.error(inner.span.pos, "operand must be a number");
                    return Type::I64;
                }
                if t == Type::F64 { Type::F64 } else { Type::I64 }
            }
            UnOp::Not => {
                if !is_scalar(&t) {
                    self.error(inner.span.pos, "operand of `!` must be scalar");
                }
                Type::I64
            }
            UnOp::BitNot => {
                if !is_integer(&t) {
                    self.error(inner.span.pos, "operand of `~` must be an integer");
                }
                Type::I64
            }
            UnOp::Deref => match Self::decay(t) {
                Type::Ptr(inner_ty) => *inner_ty,
                _ => {
                    self.error(inner.span.pos, "cannot dereference a non-pointer");
                    Type::I64
                }
            },
            UnOp::AddrOf => {
                // `&Func` is a function pointer (a function is addressable even
                // though it is not an lvalue). A local variable shadows a function
                // of the same name.
                if let ExprKind::Ident(name) = &inner.kind {
                    if self.lookup_var(name).is_none() {
                        if let Some(fp) = self.func_ptr_type(name) {
                            inner.set_ty(fp.clone());
                            return fp;
                        }
                    }
                }
                if !self.is_lvalue(inner) {
                    self.error(inner.span.pos, "cannot take the address of a non-lvalue");
                }
                Type::Ptr(Box::new(t))
            }
            UnOp::PreInc | UnOp::PreDec => {
                if !self.is_lvalue(inner) {
                    self.error(inner.span.pos, "operand of `++`/`--` must be an lvalue");
                }
                t
            }
        }
    }

    fn check_binary(&mut self, op: BinOp, lhs: &Expr, rhs: &Expr) -> Type {
        let lt = Self::decay(self.check_expr(lhs));
        let rt = Self::decay(self.check_expr(rhs));
        use BinOp::*;
        match op {
            Add | Sub | Mul | Div | Mod => {
                if !is_scalar(&lt) || !is_scalar(&rt) {
                    self.error(
                        lhs.span.pos,
                        "arithmetic requires numeric or pointer operands",
                    );
                    return Type::I64;
                }
                // pointer - pointer yields an integer offset.
                if op == Sub && is_pointer(&lt) && is_pointer(&rt) {
                    return Type::I64;
                }
                arith_result(&lt, &rt)
            }
            Eq | Ne | Lt | Gt | Le | Ge | And | Or => {
                if !is_scalar(&lt) || !is_scalar(&rt) {
                    self.error(lhs.span.pos, "comparison requires scalar operands");
                }
                Type::I64
            }
            BitAnd | BitOr | BitXor | Shl | Shr => {
                if !is_integer(&lt) || !is_integer(&rt) {
                    self.error(
                        lhs.span.pos,
                        "bitwise/shift operators require integer operands",
                    );
                }
                Type::I64
            }
        }
    }

    fn check_assign(&mut self, _op: AssignOp, target: &Expr, value: &Expr) -> Type {
        let tt = self.check_expr(target);
        let vt = self.check_expr(value);
        if !self.is_lvalue(target) {
            self.error(
                target.span.pos,
                "left-hand side of assignment is not an lvalue",
            );
        }
        self.check_assignable(&tt, &vt, value.span.pos);
        tt
    }

    fn check_call(&mut self, callee: &Expr, args: &[Expr]) -> Type {
        // Resolve argument types first (always check them).
        let argc = args.len();
        for a in args {
            self.check_expr(a);
        }
        // A direct call to a named function or builtin — unless a local variable
        // of the same name shadows it (then it's a function-pointer call).
        if let ExprKind::Ident(name) = &callee.kind {
            if self.lookup_var(name).is_none() {
                if let Some(sig) = self.funcs.get(name) {
                    let (required, total, varargs, ret) =
                        (sig.required, sig.total, sig.varargs, sig.ret.clone());
                    let ok = if varargs {
                        argc >= required
                    } else {
                        argc >= required && argc <= total
                    };
                    if !ok {
                        let expected = if varargs {
                            format!("at least {required}")
                        } else if required == total {
                            format!("{total}")
                        } else {
                            format!("{required} to {total}")
                        };
                        self.error(
                            callee.span.pos,
                            format!("function `{name}` expects {expected} argument(s), got {argc}"),
                        );
                    }
                    return ret;
                }
                self.error(
                    callee.span.pos,
                    format!("call to undeclared function `{name}`"),
                );
                return Type::I64;
            }
        }
        // Otherwise the callee is a value expression that must be a function
        // pointer (a `FuncPtr` variable, `&Func`, etc.).
        if let Type::FuncPtr { ret, params } = Self::decay(self.check_expr(callee)) {
            if argc != params.len() {
                self.error(
                    callee.span.pos,
                    format!(
                        "function pointer expects {} argument(s), got {argc}",
                        params.len()
                    ),
                );
            }
            return *ret;
        }
        self.error(callee.span.pos, "called value is not a function");
        Type::I64
    }

    fn check_index(&mut self, base: &Expr, index: &Expr) -> Type {
        let bt = Self::decay(self.check_expr(base));
        let it = self.check_expr(index);
        if !is_integer(&it) {
            self.error(index.span.pos, "array index must be an integer");
        }
        match bt {
            Type::Ptr(inner) => *inner,
            _ => {
                self.error(base.span.pos, "cannot index a non-pointer value");
                Type::I64
            }
        }
    }

    fn check_member(&mut self, base: &Expr, field: &str, arrow: bool, pos: Pos) -> Type {
        let bt = self.check_expr(base);
        // Determine the class/union name being accessed.
        let class_name = if arrow {
            match Self::decay(bt) {
                Type::Ptr(inner) => match *inner {
                    Type::Named(n) => Some(n),
                    _ => {
                        self.error(pos, "`->` requires a pointer to a class or union");
                        None
                    }
                },
                _ => {
                    self.error(pos, "`->` requires a pointer to a class or union");
                    None
                }
            }
        } else {
            match bt {
                Type::Named(n) => Some(n),
                Type::Ptr(_) => {
                    self.error(pos, "use `->` to access a member through a pointer");
                    None
                }
                _ => {
                    self.error(pos, "`.` requires a class or union value");
                    None
                }
            }
        };

        match class_name {
            Some(name) => match self.lookup_field(&name, field) {
                Some(ty) => ty,
                None => {
                    self.error(pos, format!("no field `{field}` on type `{name}`"));
                    Type::I64
                }
            },
            None => Type::I64,
        }
    }

    /// Find a field by name in a class/union, searching base classes.
    fn lookup_field(&self, class: &str, field: &str) -> Option<Type> {
        let def = self.types.get(class)?;
        // A direct field.
        if let Some((_, ty, _)) = def.fields.iter().find(|(n, _, _)| n == field) {
            return Some(ty.clone());
        }
        // A member promoted from an anonymous embedded union.
        for (n, ty, _) in &def.fields {
            if is_anon_field(n) {
                if let Type::Named(inner) = ty {
                    if let Some(t) = self.lookup_field(inner, field) {
                        return Some(t);
                    }
                }
            }
        }
        // A field inherited from a base class.
        def.base
            .as_ref()
            .and_then(|base| self.lookup_field(base, field))
    }

    /// Validate an `offset(Class.field...)` operand: the class must exist and
    /// each member along the path must resolve, descending into nested classes.
    fn check_offset(&mut self, class: &str, path: &[String], pos: Pos) {
        if !self.types.contains_key(class) {
            self.error(pos, format!("`{class}` is not a known class or union"));
            return;
        }
        let mut current = class.to_string();
        for (i, field) in path.iter().enumerate() {
            let ty = match self.lookup_field(&current, field) {
                Some(t) => t,
                None => {
                    self.error(pos, format!("no field `{field}` on type `{current}`"));
                    return;
                }
            };
            if i + 1 < path.len() {
                match ty {
                    Type::Named(n) => current = n,
                    _ => {
                        self.error(
                            pos,
                            format!("`{current}.{field}` is not a class, so `offset` cannot descend into it"),
                        );
                        return;
                    }
                }
            }
        }
    }

    fn is_lvalue(&self, expr: &Expr) -> bool {
        match &expr.kind {
            ExprKind::Ident(name) => self.lookup_var(name).is_some(),
            // `p->x` is always an lvalue; `a.x` is one only if `a` is — so a
            // member of a temporary (e.g. a class-returning call) is not.
            ExprKind::Member { base, arrow, .. } => *arrow || self.is_lvalue(base),
            ExprKind::Index { .. } => true,
            ExprKind::Unary {
                op: UnOp::Deref, ..
            } => true,
            _ => false,
        }
    }

    /// Whether a value of type `from` may be assigned to a slot of type `to`.
    /// Permissive for scalars (HolyC freely mixes integers, floats, pointers);
    /// strict only about void and class/union mismatches.
    fn check_assignable(&mut self, to: &Type, from: &Type, pos: Pos) {
        let to = Self::decay(to.clone());
        let from = Self::decay(from.clone());
        if to == from {
            return;
        }
        match (&to, &from) {
            (Type::Named(a), Type::Named(b)) if a != b => {
                self.error(pos, format!("cannot assign `{b}` to `{a}`"));
            }
            (Type::Named(a), _) => {
                self.error(pos, format!("cannot assign a scalar to class type `{a}`"));
            }
            (_, Type::Named(b)) => {
                self.error(pos, format!("cannot assign class type `{b}` to a scalar"));
            }
            // Any scalar-to-scalar (int/float/pointer) assignment is allowed.
            _ => {}
        }
    }
}

/// The labels declared directly in a statement list (one level — not inside
/// nested blocks). A `goto` can target these from anywhere within the block or
/// a nested block, matching the interpreter's label-resume scope.
fn direct_labels(stmts: &[Stmt]) -> HashSet<String> {
    stmts
        .iter()
        .filter_map(|s| match &s.kind {
            StmtKind::Label(name) => Some(name.clone()),
            _ => None,
        })
        .collect()
}

// ---- type predicates & promotion ----

fn is_integer(ty: &Type) -> bool {
    matches!(
        ty,
        Type::I8
            | Type::U8
            | Type::I16
            | Type::U16
            | Type::I32
            | Type::U32
            | Type::I64
            | Type::U64
            | Type::Bool
    )
}

fn is_arithmetic(ty: &Type) -> bool {
    is_integer(ty) || matches!(ty, Type::F64)
}

fn is_pointer(ty: &Type) -> bool {
    matches!(ty, Type::Ptr(_) | Type::Array(..) | Type::FuncPtr { .. })
}

/// Whether a field name is the generated placeholder for an anonymous embedded
/// union, whose members are promoted into the enclosing class.
pub fn is_anon_field(name: &str) -> bool {
    name.starts_with("$anon")
}

fn is_scalar(ty: &Type) -> bool {
    is_arithmetic(ty) || is_pointer(ty)
}

/// The result type of arithmetic / a ternary over two scalar operands. Floats
/// win over integers; a pointer operand makes the result that pointer; integer
/// arithmetic is performed at 64-bit width (HolyC register semantics).
fn arith_result(a: &Type, b: &Type) -> Type {
    let a = Analyzer::decay(a.clone());
    let b = Analyzer::decay(b.clone());
    if a == Type::F64 || b == Type::F64 {
        return Type::F64;
    }
    if is_pointer(&a) {
        return a;
    }
    if is_pointer(&b) {
        return b;
    }
    Type::I64
}
