//! Semantic analysis for HolyC: name resolution, type inference, and validity
//! checks over the parsed [`Program`].
//!
//! HolyC is weakly typed and C-like. The default integer is `I64`, pointers and
//! integers convert freely, and comparison and logical results are `I64`. The
//! analyzer reflects that: it is permissive about scalar conversions and focuses
//! on catching genuine mistakes:
//!
//!   * use of undeclared variables, unknown types, and unknown fields,
//!   * redeclaration of variables, parameters, fields, functions, and types,
//!   * `break`/`continue`/`case`/`default` used out of context,
//!   * `goto` to a label that does not exist in the function,
//!   * `return` that disagrees with the function's return type,
//!   * non-scalar conditions, indexing non-pointers, member access on
//!     non-aggregates, assigning to non-lvalues, and `&` of non-lvalues.
//!
//! Analysis does not stop at the first error: all errors are collected, each
//! carrying a source position.
//!
//! A call to a name that is neither a user function nor a known intrinsic is a
//! compile-time error. The core and standard libraries are not modelled yet;
//! their functions will be registered as intrinsics when they land.

use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::token::{FileInfo, Pos, Span};

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

/// Type-checks and resolves names in `program`. Returns every error found, or an
/// empty `Vec` if the program is well-formed.
pub fn check_program(program: &Program) -> Vec<SemaError> {
    let mut a = Analyzer::new();
    a.run(program);
    a.errors
}

/// Convenience wrapper over [`check_program`] returning a `Result`.
pub fn analyze(program: &Program) -> Result<(), Vec<SemaError>> {
    let errs = check_program(program);
    if errs.is_empty() { Ok(()) } else { Err(errs) }
}

/// A class or union definition, flattened for field lookup. Each field keeps its
/// source position so type-reference errors can point at it.
struct TypeDef {
    fields: Vec<(String, Type, Pos)>,
    base: Option<String>,
    /// Position of the definition, used to locate base-class errors.
    base_pos: Pos,
    /// The file this type was defined in (`Span::file`). With the file-scoped
    /// visibility default, a non-`public` type is visible only within this file.
    file: u32,
    /// Whether the type was declared `public` (visible from any file).
    is_public: bool,
}

/// A function signature.
struct FuncSig {
    ret: Type,
    /// Declared parameter types. Used to build `&Func` function-pointer types.
    params: Vec<Type>,
    /// Number of parameters that must be supplied (those without a default).
    required: usize,
    /// Total declared parameter count.
    total: usize,
    varargs: bool,
    /// Whether a definition (not just a prototype) has been seen.
    defined: bool,
    /// The file this function was first declared in (`Span::file`). With the
    /// file-scoped visibility default, a non-`public` function is callable only
    /// within this file.
    file: u32,
    /// Whether any declaration of the function was marked `public`.
    is_public: bool,
}

struct Analyzer {
    errors: Vec<SemaError>,
    types: HashMap<String, TypeDef>,
    funcs: HashMap<String, FuncSig>,
    /// Lexical variable scopes; `scopes[0]` is the global scope.
    scopes: Vec<HashMap<String, Type>>,
    /// The defining file (`Span::file`) of each global, for file-scoped visibility.
    global_files: HashMap<String, u32>,
    /// Whether each global was declared `public`.
    global_is_public: HashMap<String, bool>,
    /// Return type of the function currently being checked, if any.
    cur_ret: Option<Type>,
    loop_depth: u32,
    switch_depth: u32,
    /// Stack of label scopes: the labels declared directly in the current block
    /// and each enclosing block. A `goto` is valid iff its target is in one of
    /// these. This is the same reachability rule the interpreter uses to resolve
    /// gotos.
    label_scopes: Vec<HashSet<String>>,
    /// The program's source-file table, indexed by `Span::file`, for `_`-privacy.
    /// Empty until `run` copies it from the program.
    files: Vec<FileInfo>,
    /// The file (`Span::file`) of the top-level item currently being checked. This
    /// is the reference site for file-scoped visibility of *type* references.
    cur_file: u32,
    /// Whether the top-level item currently being checked is compiler-generated
    /// (monomorphized/synthetic). References from such code bypass visibility checks.
    in_generated: bool,
}

impl Analyzer {
    fn new() -> Self {
        Analyzer {
            errors: Vec::new(),
            types: HashMap::new(),
            funcs: HashMap::new(),
            scopes: Vec::new(),
            global_files: HashMap::new(),
            global_is_public: HashMap::new(),
            cur_ret: None,
            loop_depth: 0,
            switch_depth: 0,
            label_scopes: Vec::new(),
            files: Vec::new(),
            cur_file: 0,
            in_generated: false,
        }
    }

    fn run(&mut self, program: &Program) {
        self.files = program.files.clone();
        self.scopes.push(HashMap::new()); // global scope
        // `argc`/`argv` are scope-dependent and so are NOT global-scope names: at the top
        // level (`check_ident` with no enclosing function) they are the command line
        // (`I64 argc`, `U8 **argv`); inside a `...` function they are the varargs
        // (`I64 argc`, `I64 *argv`), declared as locals below. A non-variadic function can
        // reach neither — referencing them there is an "undeclared identifier" error.
        // The environment: `U8 **envp`, a NULL-terminated array of "KEY=VALUE" strings. It
        // has a single meaning, so unlike `argc`/`argv` it is a plain global, in scope
        // everywhere (e.g. `Getenv` walks it).
        self.scopes[0].insert(
            "envp".to_string(),
            Type::Ptr(Box::new(Type::Ptr(Box::new(Type::U8)))),
        );
        // The current task/thread context: `CTask *Fs`, holding the exception state
        // read inside `catch` (`Fs->except_ch`). `CTask` is defined in `builtin.hc`.
        self.scopes[0].insert(
            "Fs".to_string(),
            Type::Ptr(Box::new(Type::Named("CTask".to_string()))),
        );
        self.collect_types(program);
        self.collect_funcs(program);
        self.validate_type_refs();
        self.check_public_signatures(program);
        self.check_layouts(program);
        // Top-level statements form an implicit body with their own label scope,
        // so `goto` and labels work at the top level too.
        self.label_scopes.push(direct_labels(&program.items));
        for item in &program.items {
            self.check_top_item(item);
        }
        self.label_scopes.pop();
        self.scopes.pop();
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
                        file: item.span.file,
                        is_public: c.is_public,
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
                    // A prototype followed by a definition, or vice versa, is
                    // fine. Keep the entry, marking it defined if either is, and
                    // public if any declaration was `public`.
                    let defined = existing.defined || has_body;
                    let is_public = existing.is_public || f.is_public;
                    let sig = self.funcs.get_mut(&f.name).unwrap();
                    sig.defined = defined;
                    sig.is_public = is_public;
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
                            file: item.span.file,
                            is_public: f.is_public,
                        },
                    );
                }
            }
        }
    }

    /// Runs the layout pass and surfaces its errors as semantic errors. Those
    /// errors are cyclic by-value types and non-constant field array sizes.
    fn check_layouts(&mut self, program: &Program) {
        let (_, errs) = crate::layout::compute(program);
        for e in errs {
            self.error(e.pos, e.message);
        }
    }

    /// After all types are registered, confirms that field and base-class type
    /// references actually exist.
    ///
    /// Each field or base reference is checked for existence and `_`-privacy
    /// against the file of the class that declares it.
    fn validate_type_refs(&mut self) {
        // Collect the work first to avoid borrowing `self.types` while pushing
        // errors.
        let mut refs: Vec<(Type, Pos, u32)> = Vec::new();
        let mut base_refs: Vec<(String, Pos)> = Vec::new();
        let names: Vec<String> = self.types.keys().cloned().collect();
        for name in &names {
            let def = &self.types[name];
            for (_, ty, pos) in &def.fields {
                refs.push((ty.clone(), *pos, def.file));
            }
            if let Some(b) = &def.base {
                base_refs.push((b.clone(), def.base_pos));
            }
        }
        for (ty, pos, file) in refs {
            // `cur_file` drives nested checks, e.g. a global in an array dimension.
            // `file` is the field's own file, the type reference site.
            self.cur_file = file;
            self.resolve_type(&ty, pos, file);
        }
        for (b, pos) in base_refs {
            if !self.types.contains_key(&b) {
                self.error(pos, format!("unknown base type `{b}`"));
            }
        }
    }

    /// A `public` function must not expose a non-`public` type through its return type:
    /// a caller in another file can call the function but couldn't name the result. The
    /// return type's underlying named type (peeling pointers/arrays) must be `public`.
    fn check_public_signatures(&mut self, program: &Program) {
        for item in &program.items {
            // Compiler-generated functions (monomorphized generic instances) are
            // trusted: an instance is always public yet returns the user's type
            // argument, which may be private — the user already named it at the use
            // site, so it isn't an API leak. Skip them.
            if item.span.file == crate::token::GENERATED_FILE {
                continue;
            }
            if let StmtKind::Func(f) = &item.kind {
                if f.is_public {
                    if let Some(n) = self.first_private_named(&f.ret) {
                        self.error(
                            item.span.pos,
                            format!(
                                "`public` function `{}` returns non-`public` type `{n}`; \
                                 declare `{n}` `public` so callers can name the result",
                                f.name
                            ),
                        );
                    }
                }
            }
        }
    }

    /// The name of the first non-`public` named class/union reachable in `ty` by peeling
    /// pointers and arrays, or `None` if every named component is `public` (or built-in).
    /// Compiler-generated types (synthetic tuples, monomorphized instances) are always
    /// `public`, so they never trip this.
    fn first_private_named(&self, ty: &Type) -> Option<String> {
        match ty {
            Type::Named(n) => match self.types.get(n) {
                Some(td) if !td.is_public => Some(n.clone()),
                _ => None,
            },
            Type::Ptr(inner) | Type::Array(inner, _) => self.first_private_named(inner),
            _ => None,
        }
    }

    // ---- scope helpers ----

    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    /// Declares a variable in the current scope, reporting a redeclaration if the
    /// name already exists at this level. `is_public` is the global's `public`
    /// modifier; it is meaningless on locals, so a `public` local is an error.
    fn declare(&mut self, name: &str, ty: Type, pos: Pos, is_public: bool) {
        // A declaration at the global scope (only `scopes[0]` present) records its
        // defining file and publicness, so file-scoped visibility is gated on use.
        let is_global = self.scopes.len() == 1;
        if is_public && !is_global {
            self.error(pos, "`public` is only allowed on top-level declarations");
        }
        let scope = self.scopes.last_mut().unwrap();
        if scope.contains_key(name) {
            self.error(pos, format!("redeclaration of `{name}`"));
        } else {
            scope.insert(name.to_string(), ty);
            if is_global {
                self.global_files.insert(name.to_string(), self.cur_file);
                self.global_is_public.insert(name.to_string(), is_public);
            }
        }
    }

    fn lookup_var(&self, name: &str) -> Option<&Type> {
        self.scopes.iter().rev().find_map(|s| s.get(name))
    }

    // ---- type resolution & predicates ----

    /// Confirms a type's named parts exist, reporting and continuing otherwise.
    ///
    /// `ref_file` is the `Span::file` of the syntactic site that uses the type, so
    /// `_`-privacy is checked against the exact reference location. This mirrors
    /// how [`Analyzer::check_call`] handles functions.
    fn resolve_type(&mut self, ty: &Type, pos: Pos, ref_file: u32) {
        match ty {
            Type::Named(n) => match self.types.get(n) {
                None => self.error(pos, format!("unknown type `{n}`")),
                // A non-`public` type is visible only within its defining file.
                Some(td) => {
                    let (is_public, def_file) = (td.is_public, td.file);
                    self.check_visibility(is_public, def_file, ref_file, n, pos);
                }
            },
            Type::Ptr(inner) => self.resolve_type(inner, pos, ref_file),
            Type::Array(inner, dim) => {
                self.resolve_type(inner, pos, ref_file);
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

    /// Decays an array to a pointer, as happens in value contexts.
    fn decay(ty: Type) -> Type {
        decay(ty)
    }

    // ---- top-level & statements ----

    fn check_top_item(&mut self, item: &Stmt) {
        // Type references inside this item are checked for file-scoped visibility
        // against this item's file. A generated (monomorphized) item is trusted.
        self.cur_file = item.span.file;
        self.in_generated = item.span.file == crate::token::GENERATED_FILE;
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
        // The return type's reference site is the function signature, whose file is
        // the current item's file (set in `check_top_item`).
        self.resolve_type(&f.ret, Pos::new(0, 0), self.cur_file);
        self.cur_ret = Some(f.ret.clone());

        self.label_scopes.push(direct_labels(body));
        self.push_scope();
        for p in &f.params {
            self.resolve_type(&p.ty, p.span.pos, p.span.file);
            if matches!(p.ty, Type::U0) {
                self.error(p.span.pos, "parameter cannot have type U0");
            }
            if let Some(d) = &p.default {
                self.check_expr(d);
            }
            if let Some(name) = &p.name {
                self.declare(name, Self::decay(p.ty.clone()), p.span.pos, false);
            }
        }
        // A `...` function gets the implicit HolyC varargs locals: `I64 argc` (the
        // count) and `I64 *argv` (the raw 8-byte slots; pun the address for other
        // types).
        if f.varargs {
            let pos = f.params.first().map_or(Pos::new(0, 0), |p| p.span.pos);
            self.declare("argc", Type::I64, pos, false);
            self.declare("argv", Type::Ptr(Box::new(Type::I64)), pos, false);
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
            StmtKind::ShortDecl { .. } => unreachable!("deferred `:=` reached sema"),
            StmtKind::TypeSwitch { .. } => unreachable!("type switch reached sema"),
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
            StmtKind::Try { body, handler } => {
                // Each block gets its own scope, like a `Block`.
                self.label_scopes.push(direct_labels(body));
                self.push_scope();
                for s in body {
                    self.check_stmt(s);
                }
                self.pop_scope();
                self.label_scopes.pop();
                self.label_scopes.push(direct_labels(handler));
                self.push_scope();
                for s in handler {
                    self.check_stmt(s);
                }
                self.pop_scope();
                self.label_scopes.pop();
            }
            StmtKind::Throw(val) => {
                // The thrown value is stored in `I64 Fs->except_ch`, so it must be an
                // integer (a bare `throw;` re-raises the current value).
                if let Some(e) = val {
                    let t = self.check_expr(e);
                    if !is_integer(&t) {
                        self.error(e.span.pos, "`throw` value must be an integer");
                    }
                }
            }
            // Nested functions and classes are uncommon in HolyC. Check a nested
            // function body best-effort, and ignore nested class registration.
            StmtKind::Func(f) => self.check_function(f),
            StmtKind::Class(_) => {}
        }
    }

    fn check_declarator(&mut self, d: &Declarator) {
        self.resolve_type(&d.ty, d.span.pos, d.span.file);
        if matches!(d.ty, Type::U0) {
            self.error(
                d.span.pos,
                format!("variable `{}` cannot have type U0", d.name),
            );
        }
        if let Some(init) = &d.init {
            self.check_init(init, &d.ty, init.span.pos);
        }
        self.declare(&d.name, d.ty.clone(), d.span.pos, d.is_public);
    }

    /// Checks an initialiser against its declared type.
    ///
    /// A brace `InitList` is matched element-by-element against an array's element
    /// type or against a class's fields in layout order. Any other expression is
    /// checked for assignability, exactly like a plain `=` initialiser.
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

    /// Checks a designated initialiser `{.field = value, ...}` against its declared
    /// type. The target must be a class. Each designator must name an existing
    /// field, and its value is checked against that field's type.
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
        // Resolve each designator with `lookup_field` (the same lookup member access uses),
        // so a member promoted from an anonymous embedded union — e.g. `.x` of
        // `class C { union { I64 x; I64 y; }; }` — is found, matching `c.x`.
        for (name, value) in items {
            match self.lookup_field(class, name) {
                Some(fty) => self.check_init(value, &fty, value.span.pos),
                None => {
                    self.error(value.span.pos, format!("`{class}` has no field `{name}`"));
                    self.check_expr(value);
                }
            }
        }
    }

    /// The function-pointer type of a named user function, used for `&Func`.
    /// Returns `None` if `name` is not a function.
    fn func_ptr_type(&self, name: &str) -> Option<Type> {
        let sig = self.funcs.get(name)?;
        Some(Type::FuncPtr {
            ret: Box::new(sig.ret.clone()),
            params: sig.params.clone(),
        })
    }

    /// A class or union's field types in layout order: inherited (base) fields
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

    /// Enforces the placement rules for the `start:` / `end:` switch sub-labels:
    /// at most one of each, `start:` before every case, and `end:` after every
    /// case. This keeps the prologue/epilogue partition unambiguous for both
    /// backends.
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
                // A brace or tuple-literal return (`return a, b;`) is checked against
                // the tuple/aggregate return type, like an initialiser.
                if matches!(e.kind, ExprKind::InitList(_) | ExprKind::DesignatedInit(_)) {
                    self.check_init(e, &rt, e.span.pos);
                } else {
                    let vt = self.check_expr(e);
                    self.check_assignable(&rt, &vt, e.span.pos);
                }
            }
            (Some(rt), None) if rt != Type::U0 => {
                self.error(pos, "missing return value in non-void function");
            }
            // U0 with no value, or a top-level return: both fine.
            (_, Some(e)) => {
                self.check_expr(e);
            }
            _ => {}
        }
    }

    // ---- expressions: returns the inferred type ----

    /// Infers an expression's type and records it on the node. This builds the
    /// typed AST that the backends and `sizeof(expr)` rely on.
    fn check_expr(&mut self, expr: &Expr) -> Type {
        let t = self.infer(expr);
        expr.set_ty(t.clone());
        t
    }

    fn infer(&mut self, expr: &Expr) -> Type {
        match &expr.kind {
            // Deferred generic calls only live in templates; the mono pass removes
            // them before sema.
            ExprKind::GenericCall { .. } => unreachable!("generic call reached sema"),
            ExprKind::Int(_) | ExprKind::Char(_) => Type::I64,
            ExprKind::Float(_) => Type::F64,
            // String literals are U8*.
            ExprKind::Str(_) => Type::Ptr(Box::new(Type::U8)),
            ExprKind::Ident(name) => self.check_ident(name, expr.span),
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
                self.resolve_type(ty, inner.span.pos, inner.span.file);
                self.check_expr(inner);
                ty.clone()
            }
            ExprKind::Sizeof(arg) => {
                match arg {
                    SizeofArg::Type(t) => self.resolve_type(t, expr.span.pos, expr.span.file),
                    // Type-check the operand so its static type is inferred and
                    // recorded. The size is read from that type later.
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
                // A bare initializer list with no target type is not valid. It is
                // only meaningful in a declarator, handled by `check_init`.
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

    fn check_ident(&mut self, name: &str, span: Span) -> Type {
        if let Some(t) = self.lookup_var(name) {
            // Return the variable's true (undecayed) type so the typed AST is
            // accurate: e.g. `sizeof(arr)` sees the array, not a pointer. Use sites
            // (arithmetic, indexing, assignment) apply array-to-pointer decay
            // themselves.
            let t = t.clone();
            // If the name resolves to a global that is not shadowed by a local in an
            // inner scope, enforce file-scoped visibility like a function or type
            // reference. The reference file is the identifier's own `span.file`, the
            // precise site (just as `check_call` uses `callee.span.file`).
            let shadowed = self.scopes[1..].iter().any(|s| s.contains_key(name));
            let def_file = if shadowed {
                None
            } else {
                self.global_files.get(name).copied()
            };
            if let Some(df) = def_file {
                let is_public = self.global_is_public.get(name).copied().unwrap_or(false);
                self.check_visibility(is_public, df, span.file, name, span.pos);
            }
            return t;
        }
        // A bare function name acts like a call in HolyC, so give it the return
        // type.
        if let Some(sig) = self.funcs.get(name) {
            return sig.ret.clone();
        }
        // `argc`/`argv` at the top level (no enclosing function) are the command line.
        // Inside a variadic function they were found as varargs locals above; inside a
        // non-variadic function they fall through here to the undeclared error.
        if self.cur_ret.is_none() {
            match name {
                "argc" => return Type::I64,
                "argv" => return Type::Ptr(Box::new(Type::Ptr(Box::new(Type::U8)))),
                _ => {}
            }
        }
        self.error(span.pos, format!("use of undeclared identifier `{name}`"));
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
                // `&Func` is a function pointer: a function is addressable even
                // though it is not an lvalue. A local variable shadows a function of
                // the same name.
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
                // Pointer minus pointer yields an integer offset.
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

    /// Enforces file-scoped visibility. A symbol that is not `public` may be
    /// referenced only from within the file that defines it; a `public` symbol is
    /// visible everywhere. Same-file references (`def_file == ref_file`) are always
    /// allowed.
    fn check_visibility(
        &mut self,
        is_public: bool,
        def_file: u32,
        ref_file: u32,
        name: &str,
        pos: Pos,
    ) {
        // `public` symbols are visible everywhere; references from the same directory
        // are always allowed; and references from compiler-generated code (a
        // monomorphized instance or a reference carrying the `GENERATED_FILE` sentinel)
        // are trusted.
        if is_public
            || self.same_dir(def_file, ref_file)
            || self.in_generated
            || ref_file == crate::token::GENERATED_FILE
        {
            return;
        }
        let (dir, file) = match self.files.get(def_file as usize) {
            Some(f) => {
                let dir = if f.dir.is_empty() {
                    ".".to_string()
                } else {
                    f.dir.join("/")
                };
                (dir, f.display())
            }
            None => ("another".to_string(), "another file".to_string()),
        };
        self.error(
            pos,
            format!(
                "`{name}` is not `public`; it is private to the `{dir}` directory \
                 (defined in `{file}`)"
            ),
        );
    }

    /// Whether files `a` and `b` live in the same directory (directory-scoped privacy).
    /// The same file trivially qualifies. Missing file info is treated as same-directory
    /// (lenient), as before.
    fn same_dir(&self, a: u32, b: u32) -> bool {
        if a == b {
            return true;
        }
        match (self.files.get(a as usize), self.files.get(b as usize)) {
            (Some(fa), Some(fb)) => fa.dir == fb.dir,
            _ => true,
        }
    }

    fn check_call(&mut self, callee: &Expr, args: &[Expr]) -> Type {
        // Always resolve the argument types first.
        let argc = args.len();
        let arg_types: Vec<Type> = args.iter().map(|a| self.check_expr(a)).collect();
        // A direct call to a named function or builtin, unless a local variable of
        // the same name shadows it — in which case it is a function-pointer call.
        if let ExprKind::Ident(name) = &callee.kind {
            if self.lookup_var(name).is_none() {
                if let Some(sig) = self.funcs.get(name) {
                    let (required, total, varargs, ret, is_public, file, params) = (
                        sig.required,
                        sig.total,
                        sig.varargs,
                        sig.ret.clone(),
                        sig.is_public,
                        sig.file,
                        sig.params.clone(),
                    );
                    self.check_visibility(is_public, file, callee.span.file, name, callee.span.pos);
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
                    // Aggregate arguments are nominally checked against their parameter
                    // type (scalar/pointer conversions stay permissive); extras past the
                    // declared params are varargs and untyped.
                    for (i, pty) in params.iter().enumerate() {
                        if i >= argc {
                            break;
                        }
                        self.check_arg(pty, &arg_types[i], i, args[i].span.pos);
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
        // pointer: a `FuncPtr` variable, `&Func`, and so on.
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
            for (i, pty) in params.iter().enumerate() {
                if i >= argc {
                    break;
                }
                self.check_arg(pty, &arg_types[i], i, args[i].span.pos);
            }
            return *ret;
        }
        self.error(callee.span.pos, "called value is not a function");
        Type::I64
    }

    /// Check that argument `idx` (0-based) is type-compatible with its parameter. Only
    /// genuine **aggregate** mismatches are flagged — a `class`/`union` value must match
    /// its parameter's named type (nominal). Scalar and pointer arguments stay permissive
    /// (int/float/pointer conversions, `NULL`, array decay), matching `check_assignable`.
    fn check_arg(&mut self, param: &Type, arg: &Type, idx: usize, pos: Pos) {
        let p = Self::decay(param.clone());
        let a = Self::decay(arg.clone());
        if self.types_compatible(&p, &a) {
            return;
        }
        let n = idx + 1;
        match (&p, &a) {
            (Type::Named(pn), Type::Named(an)) => self.error(
                pos,
                format!("argument {n}: cannot pass `{an}` to a parameter of type `{pn}`"),
            ),
            (Type::Named(pn), _) => self.error(
                pos,
                format!("argument {n}: cannot pass a scalar to the class parameter `{pn}`"),
            ),
            (_, Type::Named(an)) => self.error(
                pos,
                format!("argument {n}: cannot pass class type `{an}` to a scalar parameter"),
            ),
            // Scalar/pointer args stay permissive (matching `check_assignable`).
            _ => {}
        }
    }

    fn check_index(&mut self, base: &Expr, index: &Expr) -> Type {
        let bt = Self::decay(self.check_expr(base));
        // Tuple indexing: a *constant* index selects a positional slot. The result
        // type depends on the index, so `t[0]` and `t[1]` may differ.
        if let Type::Named(name) = &bt {
            if crate::ast::is_tuple_name(name) {
                self.check_expr(index);
                let idx = match &index.kind {
                    ExprKind::Int(n) if *n >= 0 => *n,
                    _ => {
                        self.error(index.span.pos, "a tuple index must be a constant integer");
                        return Type::I64;
                    }
                };
                return match self.lookup_field(name, &format!("_{idx}")) {
                    Some(t) => t,
                    None => {
                        self.error(index.span.pos, format!("tuple index {idx} out of range"));
                        Type::I64
                    }
                };
            }
        }
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
        // Determine the class or union name being accessed.
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

    /// Finds a field by name in a class or union, searching base classes.
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

    /// Validates an `offset(Class.field...)` operand. The class must exist, and
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
            // `p->x` is always an lvalue. `a.x` is one only if `a` is, so a member
            // of a temporary (e.g. a class-returning call) is not.
            ExprKind::Member { base, arrow, .. } => *arrow || self.is_lvalue(base),
            ExprKind::Index { .. } => true,
            ExprKind::Unary {
                op: UnOp::Deref, ..
            } => true,
            _ => false,
        }
    }

    /// Whether two types are assignable across each other. Aggregates are **nominal**:
    /// a `class`/`union` value is compatible only with the *same* named type — two
    /// differently-named classes never assign across each other even with identical
    /// fields (matching HolyC, which is nominally typed; reinterpret via a pointer cast,
    /// `union`, or `MemCpy` instead). Identical anonymous or tuple types intern to one
    /// synthetic name, so they still match. Pointers/arrays/funcptrs compare
    /// component-wise (so an array dim written `[3]` matches `[1 + 2]`); scalars and
    /// pointers as operands are otherwise handled by [`Self::check_assignable`]'s
    /// permissive fall-through.
    fn types_compatible(&self, a: &Type, b: &Type) -> bool {
        self.compat(&Self::decay(a.clone()), &Self::decay(b.clone()))
    }

    /// The recursive core of [`Self::types_compatible`]. Inner types are compared raw (no
    /// decay), so an inline array field never matches a pointer field.
    fn compat(&self, a: &Type, b: &Type) -> bool {
        if a == b {
            return true;
        }
        match (a, b) {
            (Type::Ptr(x), Type::Ptr(y)) => self.compat(x, y),
            (Type::Array(x, dx), Type::Array(y, dy)) => {
                array_dims_equal(dx, dy) && self.compat(x, y)
            }
            (
                Type::FuncPtr {
                    ret: r1,
                    params: p1,
                },
                Type::FuncPtr {
                    ret: r2,
                    params: p2,
                },
            ) => {
                p1.len() == p2.len()
                    && self.compat(r1, r2)
                    && p1.iter().zip(p2).all(|(x, y)| self.compat(x, y))
            }
            // Nominal aggregates: a `class`/`union` matches only the same named type. (The
            // `a == b` check above already accepts identical names, including the one
            // synthetic name shared by identical anonymous/tuple types.)
            (Type::Named(x), Type::Named(y)) => x == y,
            _ => false,
        }
    }

    /// Whether a value of type `from` may be assigned to a slot of type `to`.
    /// Permissive for scalars, since HolyC freely mixes integers, floats, and
    /// pointers. Strict only about aggregate (class/union) mismatches, where
    /// [`Self::types_compatible`] allows any two **same-signature** types regardless of
    /// name.
    fn check_assignable(&mut self, to: &Type, from: &Type, pos: Pos) {
        let to = Self::decay(to.clone());
        let from = Self::decay(from.clone());
        if self.types_compatible(&to, &from) {
            return;
        }
        match (&to, &from) {
            (Type::Named(a), Type::Named(b)) => {
                self.error(pos, format!("cannot assign `{b}` to `{a}`"));
            }
            (Type::Named(a), _) => {
                self.error(pos, format!("cannot assign a scalar to class type `{a}`"));
            }
            (_, Type::Named(b)) => {
                self.error(pos, format!("cannot assign class type `{b}` to a scalar"));
            }
            // Any scalar-to-scalar assignment (int, float, pointer) is allowed.
            _ => {}
        }
    }
}

/// Whether two array dimensions denote the same length: both unsized, or both
/// constant-folding to the same value (falling back to structural expression equality
/// when they aren't constant). Distinguishes `I64 a[4]` from `I64 a[8]` when `compat`
/// compares array types (so `[3]` matches `[1 + 2]`).
fn array_dims_equal(a: &Option<Box<Expr>>, b: &Option<Box<Expr>>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(x), Some(y)) => match (crate::layout::const_eval(x), crate::layout::const_eval(y)) {
            (Ok(m), Ok(n)) => m == n,
            _ => x == y,
        },
        _ => false,
    }
}

/// The labels declared directly in a statement list, one level deep, not inside
/// nested blocks. A `goto` can target these from anywhere within the block or a
/// nested block, matching the interpreter's label-resume scope.
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

pub(crate) fn is_arithmetic(ty: &Type) -> bool {
    is_integer(ty) || matches!(ty, Type::F64)
}

pub(crate) fn is_pointer(ty: &Type) -> bool {
    matches!(ty, Type::Ptr(_) | Type::Array(..) | Type::FuncPtr { .. })
}

/// Applies array-to-pointer decay, as happens at use sites. Shared with the mono
/// pass's inference typer so both stages agree on operand types.
pub(crate) fn decay(ty: Type) -> Type {
    match ty {
        Type::Array(inner, _) => Type::Ptr(inner),
        other => other,
    }
}

/// Whether a field name is the generated placeholder for an anonymous embedded
/// union. Such a union's members are promoted into the enclosing class.
pub fn is_anon_field(name: &str) -> bool {
    name.starts_with("$anon")
}

pub(crate) fn is_scalar(ty: &Type) -> bool {
    is_arithmetic(ty) || is_pointer(ty)
}

/// The result type of arithmetic or a ternary over two scalar operands. A float
/// operand wins over integers. A pointer operand makes the result that pointer.
/// Integer arithmetic is performed at 64-bit width, matching HolyC register
/// semantics.
pub(crate) fn arith_result(a: &Type, b: &Type) -> Type {
    let a = decay(a.clone());
    let b = decay(b.clone());
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
