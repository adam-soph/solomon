//! A tree-walking interpreter for HolyC.
//!
//! It executes a parsed program directly. Run semantic analysis first; the
//! interpreter assumes a well-formed program and only reports faults it hits at
//! run time (division by zero, null dereference, missing function bodies, …).
//!
//! Value & memory model: most storage is a *cell* (`Rc<RefCell<Value>>`).
//! Variables, class fields, and array elements are cells, so `&x` is a handle
//! to a cell, `*p` reads/writes through it, and `p->field` / `a[i]` resolve to
//! cells (via [`Place::Cell`]). `MAlloc` of an integer/float element type is the
//! exception: it returns a raw byte buffer ([`Region::Heap`]) whose typed
//! accesses serialize `sizeof(T)` bytes ([`Place::Bytes`]), so the heap is
//! genuinely byte-addressable and type punning behaves like the native heap.
//! Pointer arithmetic and indexing scale by the element size on byte-heap
//! pointers and step element-by-element on cell pointers. A `union` instance is
//! likewise a shared byte buffer ([`Value::Union`]): its fields overlap and
//! alias, so writing one and reading another sees the same bytes — except
//! pointer/class union fields, which can't be serialized and are unsupported.
//!
//! HolyC's implicit print is honoured: an expression statement that is a string
//! literal prints it, and `"fmt", args…` formats and prints (a `Comma` whose
//! first element is a string).

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Write;
use std::rc::Rc;

use super::{Backend, BackendError};
use crate::ast::*;
use crate::layout::Layouts;
use crate::token::Pos;

/// A mutable storage location.
type Cell = Rc<RefCell<Value>>;
/// A lexical scope: names to their cells.
type Scope = HashMap<String, Cell>;

fn cell(v: Value) -> Cell {
    Rc::new(RefCell::new(v))
}

/// Recursively copy a class/array value so it gets independent storage.
fn deep_copy(v: &Value) -> Value {
    match v {
        Value::Class(fields) => {
            let src = fields.borrow();
            let mut out = HashMap::with_capacity(src.len());
            for (k, c) in src.iter() {
                out.insert(k.clone(), cell(deep_copy(&c.borrow())));
            }
            Value::Class(Rc::new(RefCell::new(out)))
        }
        Value::Array(elems) => {
            let src = elems.borrow();
            let out: Vec<Cell> = src.iter().map(|c| cell(deep_copy(&c.borrow()))).collect();
            Value::Array(Rc::new(RefCell::new(out)))
        }
        Value::Union(buf) => Value::Union(Rc::new(RefCell::new(buf.borrow().clone()))),
        other => other.clone(),
    }
}

/// Apply value semantics when storing into a variable, parameter, or lvalue:
/// HolyC structs are passed and assigned *by value*, so a class is deep-copied
/// (which also copies any arrays/structs nested inside it). Top-level arrays stay
/// by-reference — they decay to pointers — so they are not copied here.
fn by_value(v: Value) -> Value {
    match v {
        Value::Class(_) | Value::Union(_) => deep_copy(&v),
        other => other,
    }
}

/// Whether `e` denotes a place (an addressable lvalue) rather than a temporary
/// rvalue. Member access on a non-place (e.g. a class returned by a call) reads
/// the base's value instead of resolving it to a storage cell.
fn is_place(e: &Expr) -> bool {
    matches!(
        e.kind,
        ExprKind::Ident(_)
            | ExprKind::Member { .. }
            | ExprKind::Index { .. }
            | ExprKind::Unary {
                op: UnOp::Deref,
                ..
            }
    )
}

/// What a pointer points into. Giving pointers a region + offset (rather than a
/// bare cell) is what lets pointer arithmetic, indexing, comparison, and
/// difference work.
#[derive(Clone)]
enum Region {
    /// A single standalone cell — `&scalar`, `&field`. Only offset 0 is valid.
    Scalar(Cell),
    /// An array's element cells; the pointer's index selects one.
    Array(Rc<RefCell<Vec<Cell>>>),
    /// A raw, byte-addressable heap buffer (from `MAlloc` of an integer/float
    /// element type, or an untyped allocation). A pointer's `index` into it is a
    /// **byte** offset, and a typed access reads/writes `sizeof(T)` bytes — so
    /// the same buffer viewed through different scalar pointer types (type
    /// punning) behaves like the native byte heap. Class/pointer-element
    /// allocations use `Array` instead (cells, no serialization needed).
    Heap(Rc<RefCell<Vec<u8>>>),
}

impl Region {
    /// The cell at `index`, or `None` if out of range / not cell-backed.
    fn cell_at(&self, index: i64) -> Option<Cell> {
        match self {
            Region::Scalar(c) => (index == 0).then(|| c.clone()),
            Region::Array(rc) if index >= 0 => rc.borrow().get(index as usize).cloned(),
            Region::Array(_) | Region::Heap(_) => None,
        }
    }

    /// Whether two regions are the same underlying storage.
    fn same(&self, other: &Region) -> bool {
        match (self, other) {
            (Region::Scalar(a), Region::Scalar(b)) => Rc::ptr_eq(a, b),
            (Region::Array(a), Region::Array(b)) => Rc::ptr_eq(a, b),
            (Region::Heap(a), Region::Heap(b)) => Rc::ptr_eq(a, b),
            _ => false,
        }
    }

    /// A stable address, used to order/compare pointers into different regions.
    fn base_addr(&self) -> usize {
        match self {
            Region::Scalar(c) => Rc::as_ptr(c) as *const () as usize,
            Region::Array(rc) => Rc::as_ptr(rc) as *const () as usize,
            Region::Heap(rc) => Rc::as_ptr(rc) as *const () as usize,
        }
    }
}

/// A writable storage location: either a value cell or a typed slot inside a raw
/// byte heap buffer. Reads/writes on a byte slot pack/unpack `sizeof(ty)` bytes.
enum Place {
    Cell(Cell),
    Bytes {
        buf: Rc<RefCell<Vec<u8>>>,
        off: usize,
        ty: Type,
    },
}

impl Place {
    fn load(&self, pos: Pos) -> Result<Value, BackendError> {
        match self {
            Place::Cell(c) => Ok(c.borrow().clone()),
            Place::Bytes { buf, off, ty } => {
                let n = scalar_byte_size(ty);
                let bytes = buf.borrow();
                if off + n > bytes.len() {
                    return Err(BackendError::at(pos, "heap read out of bounds"));
                }
                let mut le = [0u8; 8];
                le[..n].copy_from_slice(&bytes[*off..*off + n]);
                if matches!(ty, Type::F64) {
                    Ok(Value::Float(f64::from_le_bytes(le)))
                } else {
                    // Sign-extend signed types from their width to 64 bits.
                    let mut v = i64::from_le_bytes(le);
                    if is_signed_scalar(ty) && n < 8 {
                        let shift = (8 - n) * 8;
                        v = (v << shift) >> shift;
                    }
                    Ok(Value::Int(v))
                }
            }
        }
    }

    fn store(&self, v: Value, pos: Pos) -> Result<Value, BackendError> {
        match self {
            Place::Cell(c) => {
                let stored = by_value(v);
                *c.borrow_mut() = stored.clone();
                Ok(stored)
            }
            Place::Bytes { buf, off, ty } => {
                let n = scalar_byte_size(ty);
                let le = if matches!(ty, Type::F64) {
                    v.as_f64().unwrap_or(0.0).to_le_bytes()
                } else {
                    v.as_i64()
                        .ok_or_else(|| {
                            BackendError::at(pos, "storing a non-scalar into a heap byte buffer")
                        })?
                        .to_le_bytes()
                };
                let mut bytes = buf.borrow_mut();
                if off + n > bytes.len() {
                    return Err(BackendError::at(pos, "heap write out of bounds"));
                }
                bytes[*off..*off + n].copy_from_slice(&le[..n]);
                Ok(v)
            }
        }
    }
}

/// A pointer value: a region plus an element offset into it.
#[derive(Clone)]
pub struct PtrVal {
    region: Region,
    index: i64,
}

impl PtrVal {
    fn offset(&self, by: i64) -> PtrVal {
        PtrVal {
            region: self.region.clone(),
            index: self.index + by,
        }
    }

    /// The cell this pointer currently addresses.
    fn target(&self, pos: Pos) -> Result<Cell, BackendError> {
        self.region
            .cell_at(self.index)
            .ok_or_else(|| BackendError::at(pos, "pointer dereference out of bounds"))
    }
}

/// A runtime value.
#[derive(Clone)]
pub enum Value {
    /// The unit value of `U0` expressions / functions.
    Void,
    Int(i64),
    Float(f64),
    Str(Rc<String>),
    /// A pointer: `None` is null.
    Ptr(Option<PtrVal>),
    /// A class instance: field name -> cell.
    Class(Rc<RefCell<HashMap<String, Cell>>>),
    /// A `union` instance: a raw byte buffer all the fields share. Field access
    /// reads/writes `sizeof(field)` bytes at the field's offset, so overlapping
    /// fields alias (type punning) like the native backend.
    Union(Rc<RefCell<Vec<u8>>>),
    /// An array: a list of element cells.
    Array(Rc<RefCell<Vec<Cell>>>),
    /// A function pointer: the name of the function it refers to (`&Func`).
    Func(String),
}

impl Value {
    fn truthy(&self) -> bool {
        match self {
            Value::Int(i) => *i != 0,
            Value::Float(f) => *f != 0.0,
            Value::Ptr(p) => p.is_some(),
            Value::Str(_)
            | Value::Class(_)
            | Value::Union(_)
            | Value::Array(_)
            | Value::Func(_) => true,
            Value::Void => false,
        }
    }

    /// Coerce to an integer where it makes sense (for indices, switch values,
    /// bitwise ops, …). Floats truncate; a null pointer is 0.
    fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            Value::Float(f) => Some(*f as i64),
            Value::Ptr(None) => Some(0),
            _ => None,
        }
    }

    fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Int(i) => Some(*i as f64),
            Value::Float(f) => Some(*f),
            _ => None,
        }
    }

    fn is_float(&self) -> bool {
        matches!(self, Value::Float(_))
    }
}

impl std::fmt::Debug for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Void => write!(f, "Void"),
            Value::Int(i) => write!(f, "Int({i})"),
            Value::Float(x) => write!(f, "Float({x})"),
            Value::Str(s) => write!(f, "Str({s:?})"),
            Value::Ptr(None) => write!(f, "Ptr(null)"),
            Value::Ptr(Some(_)) => write!(f, "Ptr(..)"),
            Value::Class(_) => write!(f, "Class{{..}}"),
            Value::Union(b) => write!(f, "Union[{}]", b.borrow().len()),
            Value::Array(a) => write!(f, "Array[{}]", a.borrow().len()),
            Value::Func(name) => write!(f, "Func({name})"),
        }
    }
}

/// Non-local control flow produced by executing a statement.
enum Flow {
    Normal,
    Break,
    Continue,
    Return(Value),
    Goto(String),
}

/// Function-local scope stack (block nesting). Globals live on the interpreter.
struct Env {
    scopes: Vec<Scope>,
}

impl Env {
    fn func() -> Self {
        Env {
            scopes: vec![Scope::new()],
        }
    }
    fn top_level() -> Self {
        Env { scopes: Vec::new() }
    }
}

pub struct Interpreter<W: Write> {
    out: W,
    globals: Scope,
    /// Functions are stored behind `Rc` so a call clones only a handle, not the
    /// whole body AST.
    funcs: HashMap<String, Rc<FuncDef>>,
    classes: HashMap<String, ClassDef>,
    layouts: Layouts,
    /// `RandU64` PRNG state (splitmix64); starts at 0 to match the native
    /// backend's zero-initialised global.
    rng_state: u64,
}

impl<W: Write> Interpreter<W> {
    pub fn new(out: W) -> Self {
        let mut globals = Scope::new();
        // HolyC predefined constants.
        globals.insert("NULL".into(), cell(Value::Ptr(None)));
        globals.insert("TRUE".into(), cell(Value::Int(1)));
        globals.insert("FALSE".into(), cell(Value::Int(0)));
        Interpreter {
            out,
            globals,
            funcs: HashMap::new(),
            classes: HashMap::new(),
            layouts: Layouts::empty(),
            rng_state: 0,
        }
    }

    /// Consume the interpreter and recover its output sink.
    pub fn into_output(self) -> W {
        self.out
    }

    fn io(&self, e: std::io::Error) -> BackendError {
        BackendError::new(format!("output error: {e}"), None)
    }

    // ---- name resolution ----

    fn resolve(&self, env: &Env, name: &str) -> Option<Cell> {
        for s in env.scopes.iter().rev() {
            if let Some(c) = s.get(name) {
                return Some(c.clone());
            }
        }
        self.globals.get(name).cloned()
    }

    fn declare(&mut self, env: &mut Env, name: &str, value: Value) {
        let c = cell(value);
        match env.scopes.last_mut() {
            Some(scope) => {
                scope.insert(name.to_string(), c);
            }
            None => {
                self.globals.insert(name.to_string(), c);
            }
        }
    }

    // ---- top-level driver ----

    fn run_program(&mut self, program: &Program) -> Result<(), BackendError> {
        // Compute type layouts up front so `sizeof` reports real sizes.
        self.layouts = crate::layout::compute(program).0;

        // Register all functions and classes first, so calls can resolve
        // regardless of definition order (and top-level `Main;` works).
        for item in &program.items {
            match &item.kind {
                StmtKind::Func(f) => {
                    self.funcs.insert(f.name.clone(), Rc::new(f.clone()));
                }
                StmtKind::Class(c) => {
                    self.classes.insert(c.name.clone(), c.clone());
                }
                _ => {}
            }
        }

        // Execute the top-level statements as one body (function/class items are
        // no-ops here, already registered). Using exec_stmts gives top-level
        // `goto`/labels the same resume behaviour as any block.
        let mut env = Env::top_level();
        self.exec_stmts(&program.items, &mut env)?;
        self.out.flush().map_err(|e| self.io(e))?;
        Ok(())
    }

    // ---- statements ----

    fn exec_stmt(&mut self, s: &Stmt, env: &mut Env) -> Result<Flow, BackendError> {
        match &s.kind {
            StmtKind::Empty
            | StmtKind::Label(_)
            | StmtKind::Include(_)
            | StmtKind::Func(_)
            | StmtKind::Class(_)
            | StmtKind::Case { .. }
            | StmtKind::Default
            | StmtKind::SwitchStart
            | StmtKind::SwitchEnd => Ok(Flow::Normal),

            StmtKind::Expr(e) => {
                self.exec_expr_stmt(e, env)?;
                Ok(Flow::Normal)
            }

            StmtKind::VarDecl { decls } => {
                for d in decls {
                    let v = match &d.init {
                        Some(init) => self.eval_init(init, &d.ty, env)?,
                        None => self.default_value(&d.ty, env)?,
                    };
                    self.declare(env, &d.name, v);
                }
                Ok(Flow::Normal)
            }

            StmtKind::Block(stmts) => {
                env.scopes.push(Scope::new());
                let flow = self.exec_stmts(stmts, env)?;
                env.scopes.pop();
                Ok(flow)
            }

            StmtKind::If { cond, then, else_ } => {
                if self.eval(cond, env)?.truthy() {
                    self.exec_stmt(then, env)
                } else if let Some(e) = else_ {
                    self.exec_stmt(e, env)
                } else {
                    Ok(Flow::Normal)
                }
            }

            StmtKind::While { cond, body } => {
                while self.eval(cond, env)?.truthy() {
                    match self.exec_stmt(body, env)? {
                        Flow::Normal | Flow::Continue => {}
                        Flow::Break => break,
                        other => return Ok(other),
                    }
                }
                Ok(Flow::Normal)
            }

            StmtKind::DoWhile { body, cond } => {
                loop {
                    match self.exec_stmt(body, env)? {
                        Flow::Normal | Flow::Continue => {}
                        Flow::Break => break,
                        other => return Ok(other),
                    }
                    if !self.eval(cond, env)?.truthy() {
                        break;
                    }
                }
                Ok(Flow::Normal)
            }

            StmtKind::For {
                init,
                cond,
                step,
                body,
            } => {
                env.scopes.push(Scope::new());
                if let Some(i) = init {
                    self.exec_stmt(i, env)?;
                }
                let mut flow = Flow::Normal;
                loop {
                    if let Some(c) = cond {
                        if !self.eval(c, env)?.truthy() {
                            break;
                        }
                    }
                    match self.exec_stmt(body, env)? {
                        Flow::Normal | Flow::Continue => {}
                        Flow::Break => break,
                        other => {
                            flow = other;
                            break;
                        }
                    }
                    if let Some(st) = step {
                        self.eval(st, env)?;
                    }
                }
                env.scopes.pop();
                Ok(flow)
            }

            StmtKind::Switch { cond, body } => self.exec_switch(cond, body, env),

            StmtKind::Break => Ok(Flow::Break),
            StmtKind::Continue => Ok(Flow::Continue),
            StmtKind::Return(opt) => {
                let v = match opt {
                    Some(e) => self.eval(e, env)?,
                    None => Value::Void,
                };
                Ok(Flow::Return(v))
            }
            StmtKind::Goto(label) => Ok(Flow::Goto(label.clone())),
        }
    }

    /// Execute a statement list. A `goto` whose target label is a direct member
    /// of this list resumes execution there; otherwise the goto propagates to
    /// the enclosing list (so labels in the current or any enclosing block are
    /// reachable). Semantic analysis enforces the same scope rule, so an
    /// unresolved goto reaching the function body is a genuine error.
    fn exec_stmts(&mut self, stmts: &[Stmt], env: &mut Env) -> Result<Flow, BackendError> {
        let mut i = 0;
        while i < stmts.len() {
            match self.exec_stmt(&stmts[i], env)? {
                Flow::Normal => i += 1,
                Flow::Goto(label) => match label_index(stmts, &label) {
                    Some(idx) => i = idx,
                    None => return Ok(Flow::Goto(label)),
                },
                other => return Ok(other),
            }
        }
        Ok(Flow::Normal)
    }

    /// Run `stmts[from..to]` in sequence, returning the first non-`Normal` flow
    /// (a `break` surfaces as `Flow::Break`). Unlike `exec_stmts`, a `goto` is
    /// not resolved here — it bubbles to the enclosing block, matching how a
    /// switch body propagates control.
    fn exec_stmts_range(
        &mut self,
        stmts: &[Stmt],
        from: usize,
        to: usize,
        env: &mut Env,
    ) -> Result<Flow, BackendError> {
        let mut i = from;
        while i < to {
            match self.exec_stmt(&stmts[i], env)? {
                Flow::Normal => i += 1,
                other => return Ok(other),
            }
        }
        Ok(Flow::Normal)
    }

    /// Execute a function body and reduce its control flow to a return value.
    fn exec_func_body(&mut self, stmts: &[Stmt], env: &mut Env) -> Result<Value, BackendError> {
        match self.exec_stmts(stmts, env)? {
            Flow::Return(v) => Ok(v),
            Flow::Normal => Ok(Value::Void),
            Flow::Goto(label) => Err(BackendError::new(
                format!("goto to undefined label `{label}`"),
                None,
            )),
            Flow::Break | Flow::Continue => Err(BackendError::new(
                "`break`/`continue` outside of a loop",
                None,
            )),
        }
    }

    fn exec_switch(
        &mut self,
        cond: &Expr,
        body: &Stmt,
        env: &mut Env,
    ) -> Result<Flow, BackendError> {
        let v = self.to_i64_eval(cond, cond.span.pos, env)?;
        let StmtKind::Block(stmts) = &body.kind else {
            // A non-block switch body is unusual; just run it.
            return self.exec_stmt(body, env);
        };

        // HolyC `start:` / `end:` sub-labels split the body into an optional
        // prologue (runs on entry, before dispatch) and epilogue (reached by
        // fall-through; a `break` skips it). Sema has already checked that
        // `start:` precedes every case and `end:` follows every case.
        let start_label = stmts
            .iter()
            .position(|s| matches!(s.kind, StmtKind::SwitchStart));
        let first_case = stmts
            .iter()
            .position(|s| matches!(s.kind, StmtKind::Case { .. } | StmtKind::Default));
        let end_label = stmts
            .iter()
            .position(|s| matches!(s.kind, StmtKind::SwitchEnd));

        env.scopes.push(Scope::new());

        // Prologue: statements from `start:` up to the first case.
        if let Some(sl) = start_label {
            let stop = first_case.unwrap_or(stmts.len());
            match self.exec_stmts_range(stmts, sl + 1, stop, env)? {
                Flow::Normal => {}
                Flow::Break => {
                    env.scopes.pop();
                    return Ok(Flow::Normal);
                }
                other => {
                    env.scopes.pop();
                    return Ok(other);
                }
            }
        }

        // Find the matching `case`, or `default`.
        let mut matched = None;
        let mut default_idx = None;
        for (i, s) in stmts.iter().enumerate() {
            match &s.kind {
                StmtKind::Case { lo, hi } => {
                    let lo = self.to_i64_eval(lo, lo.span.pos, env)?;
                    let hit = match hi {
                        Some(h) => {
                            let h = self.to_i64_eval(h, h.span.pos, env)?;
                            v >= lo && v <= h
                        }
                        None => v == lo,
                    };
                    if hit {
                        matched = Some(i);
                        break;
                    }
                }
                StmtKind::Default => default_idx = Some(i),
                _ => {}
            }
        }

        // No case matched: run the epilogue (if any), else nothing.
        let i = match matched.or(default_idx) {
            Some(i) => i,
            None => match end_label {
                Some(el) => el,
                None => {
                    env.scopes.pop();
                    return Ok(Flow::Normal);
                }
            },
        };

        // Execute from the chosen label, falling through (into the epilogue)
        // until `break`/end.
        let flow = match self.exec_stmts_range(stmts, i, stmts.len(), env)? {
            Flow::Break => Flow::Normal,
            other => other,
        };
        env.scopes.pop();
        Ok(flow)
    }

    /// HolyC implicit print at statement level.
    fn exec_expr_stmt(&mut self, e: &Expr, env: &mut Env) -> Result<(), BackendError> {
        match &e.kind {
            ExprKind::Str(s) => {
                write!(self.out, "{s}").map_err(|err| self.io(err))?;
            }
            ExprKind::Comma(items) => {
                let vals = items
                    .iter()
                    .map(|it| self.eval(it, env))
                    .collect::<Result<Vec<_>, _>>()?;
                self.print_formatted(&vals, e.span.pos)?;
            }
            _ => {
                self.eval(e, env)?;
            }
        }
        Ok(())
    }

    // ---- expressions (rvalue) ----

    fn eval(&mut self, e: &Expr, env: &mut Env) -> Result<Value, BackendError> {
        let pos = e.span.pos;
        match &e.kind {
            ExprKind::Int(v) | ExprKind::Char(v) => Ok(Value::Int(*v)),
            ExprKind::Float(v) => Ok(Value::Float(*v)),
            ExprKind::Str(s) => Ok(Value::Str(Rc::new(s.clone()))),
            ExprKind::Ident(name) => self.eval_ident(name, pos, env),

            // A dereference read may target a raw byte heap slot, so it goes
            // through `eval_place` (which serializes scalars) rather than always
            // resolving to a cell.
            ExprKind::Unary {
                op: UnOp::Deref, ..
            } => self.eval_place(e, env)?.load(pos),
            ExprKind::Unary { op, expr } => self.eval_unary(*op, expr, pos, env),
            ExprKind::Postfix { op, expr } => self.eval_postfix(*op, expr, pos, env),
            ExprKind::Binary { op, lhs, rhs } => self.eval_binary(*op, lhs, rhs, pos, env),
            ExprKind::Assign { op, target, value } => {
                self.eval_assign(*op, target, value, pos, env)
            }

            ExprKind::Ternary { cond, then, else_ } => {
                if self.eval(cond, env)?.truthy() {
                    self.eval(then, env)
                } else {
                    self.eval(else_, env)
                }
            }

            ExprKind::Call { callee, args } => self.eval_call(callee, args, env),
            ExprKind::Member { base, field, arrow } => {
                if let Some((buf, off, fty)) = self.union_field(base, field, *arrow, pos, env)? {
                    return self.read_union_field(buf, off, &fty, pos);
                }
                self.eval_place(e, env)?.load(pos)
            }
            ExprKind::Index { .. } => self.eval_place(e, env)?.load(pos),
            ExprKind::Cast { ty, expr } => {
                let v = self.eval(expr, env)?;
                Ok(cast_value(ty, v))
            }
            ExprKind::Sizeof(arg) => {
                let ty = match arg {
                    SizeofArg::Type(t) => t.clone(),
                    // `sizeof(expr)` uses the expression's statically inferred
                    // type (filled in by semantic analysis).
                    SizeofArg::Expr(e) => e.ty().ok_or_else(|| {
                        BackendError::at(
                            pos,
                            "sizeof operand has no inferred type (run semantic analysis first)",
                        )
                    })?,
                };
                Ok(Value::Int(self.size_of_type(&ty, env)?))
            }
            ExprKind::Offset { class, path } => {
                let off = self.layouts.nested_offset_of(class, path).ok_or_else(|| {
                    BackendError::at(
                        pos,
                        format!("cannot compute offset of `{class}.{}`", path.join(".")),
                    )
                })?;
                Ok(Value::Int(off as i64))
            }
            ExprKind::InitList(_) => Err(BackendError::at(
                pos,
                "an initializer list is only valid as a variable initializer",
            )),
            ExprKind::DesignatedInit(_) => Err(BackendError::at(
                pos,
                "a designated initializer is only valid as a variable initializer",
            )),
            ExprKind::Comma(items) => {
                let mut last = Value::Void;
                for it in items {
                    last = self.eval(it, env)?;
                }
                Ok(last)
            }
        }
    }

    fn eval_ident(&mut self, name: &str, pos: Pos, env: &mut Env) -> Result<Value, BackendError> {
        if let Some(c) = self.resolve(env, name) {
            return Ok(c.borrow().clone());
        }
        // A bare function name invokes it (HolyC: `Main;` calls Main).
        if self.funcs.contains_key(name) {
            return self.call(name, Vec::new(), pos);
        }
        Err(BackendError::at(pos, format!("undefined symbol `{name}`")))
    }

    fn eval_unary(
        &mut self,
        op: UnOp,
        expr: &Expr,
        pos: Pos,
        env: &mut Env,
    ) -> Result<Value, BackendError> {
        match op {
            UnOp::Neg => match self.eval(expr, env)? {
                Value::Float(f) => Ok(Value::Float(-f)),
                v => Ok(Value::Int(-self.to_i64(v, pos)?)),
            },
            UnOp::Pos => self.eval(expr, env),
            UnOp::Not => Ok(Value::Int(i64::from(!self.eval(expr, env)?.truthy()))),
            UnOp::BitNot => Ok(Value::Int(!self.to_i64_eval(expr, pos, env)?)),
            UnOp::Deref => {
                let v = self.eval(expr, env)?;
                Ok(deref_to_cell(&v, pos)?.borrow().clone())
            }
            UnOp::AddrOf => {
                // `&Func` is a function pointer (unless a variable shadows the
                // name).
                if let ExprKind::Ident(name) = &expr.kind {
                    if self.resolve(env, name).is_none() && self.funcs.contains_key(name) {
                        return Ok(Value::Func(name.clone()));
                    }
                }
                Ok(Value::Ptr(Some(self.eval_addr(expr, env)?)))
            }
            UnOp::PreInc | UnOp::PreDec => {
                let delta = if matches!(op, UnOp::PreInc) { 1 } else { -1 };
                let place = self.eval_place(expr, env)?;
                let nv = self.step_value(&place.load(pos)?, delta, expr, env, pos)?;
                place.store(nv.clone(), pos)?;
                Ok(nv)
            }
        }
    }

    fn eval_postfix(
        &mut self,
        op: PostOp,
        expr: &Expr,
        pos: Pos,
        env: &mut Env,
    ) -> Result<Value, BackendError> {
        let delta = if matches!(op, PostOp::Inc) { 1 } else { -1 };
        let place = self.eval_place(expr, env)?;
        let old = place.load(pos)?;
        let nv = self.step_value(&old, delta, expr, env, pos)?;
        place.store(nv, pos)?;
        Ok(old)
    }

    fn eval_binary(
        &mut self,
        op: BinOp,
        lhs: &Expr,
        rhs: &Expr,
        pos: Pos,
        env: &mut Env,
    ) -> Result<Value, BackendError> {
        // Logical operators short-circuit.
        match op {
            BinOp::And => {
                if !self.eval(lhs, env)?.truthy() {
                    return Ok(Value::Int(0));
                }
                return Ok(Value::Int(i64::from(self.eval(rhs, env)?.truthy())));
            }
            BinOp::Or => {
                if self.eval(lhs, env)?.truthy() {
                    return Ok(Value::Int(1));
                }
                return Ok(Value::Int(i64::from(self.eval(rhs, env)?.truthy())));
            }
            _ => {}
        }
        let l = self.eval(lhs, env)?;
        let r = self.eval(rhs, env)?;
        // Heap (byte-addressed) pointer arithmetic scales by the element size;
        // cell pointers and numbers fall through to `apply_binop`.
        if let Some(v) = self.heap_ptr_arith(op, &l, lhs, &r, rhs, env)? {
            return Ok(v);
        }
        // Signedness drives `>> / %` (left operand) and the relational compares
        // (unsigned if either operand is unsigned — C's usual conversions).
        let signed = match op {
            BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
                expr_is_signed(lhs) && expr_is_signed(rhs)
            }
            _ => expr_is_signed(lhs),
        };
        self.apply_binop(op, l, r, signed, pos)
    }

    fn apply_binop(
        &self,
        op: BinOp,
        l: Value,
        r: Value,
        signed: bool,
        pos: Pos,
    ) -> Result<Value, BackendError> {
        use BinOp::*;

        // Pointer-aware paths (arrays decay to pointers). A null pointer reads as
        // the integer 0, so it falls through to the numeric paths below.
        let lp = as_pointer(&l);
        let rp = as_pointer(&r);
        match op {
            Add => {
                if let Some(Some(pv)) = &lp {
                    if let Some(n) = r.as_i64() {
                        return Ok(Value::Ptr(Some(pv.offset(n))));
                    }
                }
                if let Some(Some(pv)) = &rp {
                    if let Some(n) = l.as_i64() {
                        return Ok(Value::Ptr(Some(pv.offset(n))));
                    }
                }
            }
            Sub => {
                if let (Some(Some(a)), Some(Some(b))) = (&lp, &rp) {
                    if a.region.same(&b.region) {
                        return Ok(Value::Int(a.index - b.index));
                    }
                    return Err(BackendError::at(
                        pos,
                        "subtracting pointers into different objects",
                    ));
                }
                if let Some(Some(pv)) = &lp {
                    if let Some(n) = r.as_i64() {
                        return Ok(Value::Ptr(Some(pv.offset(-n))));
                    }
                }
            }
            Lt | Gt | Le | Ge => {
                if let (Some(a), Some(b)) = (&lp, &rp) {
                    let (ka, kb) = (ptr_key(a), ptr_key(b));
                    let v = match op {
                        Lt => ka < kb,
                        Gt => ka > kb,
                        Le => ka <= kb,
                        Ge => ka >= kb,
                        _ => unreachable!(),
                    };
                    return Ok(Value::Int(i64::from(v)));
                }
            }
            _ => {}
        }

        match op {
            Add | Sub | Mul | Div | Mod => {
                if l.is_float() || r.is_float() {
                    let a = l.as_f64().ok_or_else(|| self.num_err(pos))?;
                    let b = r.as_f64().ok_or_else(|| self.num_err(pos))?;
                    let v = match op {
                        Add => a + b,
                        Sub => a - b,
                        Mul => a * b,
                        Div => {
                            if b == 0.0 {
                                return Err(BackendError::at(pos, "division by zero"));
                            }
                            a / b
                        }
                        Mod => a % b,
                        _ => unreachable!(),
                    };
                    Ok(Value::Float(v))
                } else {
                    let a = l.as_i64().ok_or_else(|| self.num_err(pos))?;
                    let b = r.as_i64().ok_or_else(|| self.num_err(pos))?;
                    let v = match op {
                        Add => a.wrapping_add(b),
                        Sub => a.wrapping_sub(b),
                        Mul => a.wrapping_mul(b),
                        // `/` and `%` follow the left operand's signedness.
                        Div => {
                            if b == 0 {
                                return Err(BackendError::at(pos, "division by zero"));
                            }
                            if signed {
                                a.wrapping_div(b)
                            } else {
                                ((a as u64) / (b as u64)) as i64
                            }
                        }
                        Mod => {
                            if b == 0 {
                                return Err(BackendError::at(pos, "division by zero"));
                            }
                            if signed {
                                a.wrapping_rem(b)
                            } else {
                                ((a as u64) % (b as u64)) as i64
                            }
                        }
                        _ => unreachable!(),
                    };
                    Ok(Value::Int(v))
                }
            }
            Eq => Ok(Value::Int(i64::from(values_equal(&l, &r)))),
            Ne => Ok(Value::Int(i64::from(!values_equal(&l, &r)))),
            Lt | Gt | Le | Ge => {
                // Float operands compare as f64; integers compare at full 64-bit
                // width (an f64 compare would lose precision past 2^53), signed or
                // unsigned per the operands' types.
                let v = if l.is_float() || r.is_float() {
                    let a = l.as_f64().ok_or_else(|| self.num_err(pos))?;
                    let b = r.as_f64().ok_or_else(|| self.num_err(pos))?;
                    match op {
                        Lt => a < b,
                        Gt => a > b,
                        Le => a <= b,
                        Ge => a >= b,
                        _ => unreachable!(),
                    }
                } else {
                    let a = l.as_i64().ok_or_else(|| self.num_err(pos))?;
                    let b = r.as_i64().ok_or_else(|| self.num_err(pos))?;
                    if signed {
                        match op {
                            Lt => a < b,
                            Gt => a > b,
                            Le => a <= b,
                            Ge => a >= b,
                            _ => unreachable!(),
                        }
                    } else {
                        let (a, b) = (a as u64, b as u64);
                        match op {
                            Lt => a < b,
                            Gt => a > b,
                            Le => a <= b,
                            Ge => a >= b,
                            _ => unreachable!(),
                        }
                    }
                };
                Ok(Value::Int(i64::from(v)))
            }
            BitAnd | BitOr | BitXor | Shl | Shr => {
                let a = l.as_i64().ok_or_else(|| self.num_err(pos))?;
                let b = r.as_i64().ok_or_else(|| self.num_err(pos))?;
                let v = match op {
                    BitAnd => a & b,
                    BitOr => a | b,
                    BitXor => a ^ b,
                    Shl => a.wrapping_shl(b as u32),
                    // `>>` is arithmetic for a signed left operand, logical for
                    // unsigned (C semantics) — matching the native backend.
                    Shr if signed => a.wrapping_shr(b as u32),
                    Shr => (a as u64).wrapping_shr(b as u32) as i64,
                    _ => unreachable!(),
                };
                Ok(Value::Int(v))
            }
            And | Or => unreachable!("handled with short-circuiting"),
        }
    }

    /// If `e` is a `MAlloc(size)` call destined for a `T*` pointer, allocate a
    /// buffer matching how the native heap is laid out: a raw byte buffer for an
    /// integer/float element type (so type punning works) or `size / sizeof(T)`
    /// element-typed cells for a class/pointer element (so `Pt *p =
    /// MAlloc(sizeof(Pt)*n)` holds class elements). Returns `None` for anything
    /// else, leaving normal evaluation to handle it.
    fn try_typed_malloc(
        &mut self,
        e: &Expr,
        elem: &Type,
        env: &mut Env,
    ) -> Result<Option<Value>, BackendError> {
        let ExprKind::Call { callee, args } = &e.kind else {
            return Ok(None);
        };
        let ExprKind::Ident(name) = &callee.kind else {
            return Ok(None);
        };
        if name != "MAlloc" || args.len() != 1 || self.resolve(env, name).is_some() {
            return Ok(None);
        }
        let bytes = self.to_i64_eval(&args[0], e.span.pos, env)?.max(0);
        Ok(Some(self.alloc(bytes, elem, env)?))
    }

    /// Allocate a heap buffer of `bytes` bytes for element type `elem`: a raw
    /// byte region for integer/float scalars, else a region of `bytes /
    /// sizeof(elem)` element cells.
    fn alloc(&mut self, bytes: i64, elem: &Type, env: &mut Env) -> Result<Value, BackendError> {
        let bytes = bytes.max(0);
        let region = if is_byte_heap_elem(elem) {
            Region::Heap(Rc::new(RefCell::new(vec![0u8; bytes as usize])))
        } else {
            let esize = self.size_of_type(elem, env)?.max(1);
            let count = (bytes / esize) as usize;
            let cells = (0..count)
                .map(|_| self.default_value(elem, env).map(cell))
                .collect::<Result<Vec<_>, _>>()?;
            Region::Array(Rc::new(RefCell::new(cells)))
        };
        Ok(Value::Ptr(Some(PtrVal { region, index: 0 })))
    }

    fn eval_assign(
        &mut self,
        op: AssignOp,
        target: &Expr,
        value: &Expr,
        pos: Pos,
        env: &mut Env,
    ) -> Result<Value, BackendError> {
        let place = self.eval_place(target, env)?;
        let rhs = match target.ty() {
            Some(Type::Ptr(elem)) if op == AssignOp::Assign => {
                match self.try_typed_malloc(value, &elem, env)? {
                    Some(v) => v,
                    None => self.eval(value, env)?,
                }
            }
            _ => self.eval(value, env)?,
        };
        let newv = match op {
            AssignOp::Assign => rhs,
            _ => {
                let cur = place.load(pos)?;
                self.apply_binop(compound_binop(op), cur, rhs, expr_is_signed(target), pos)?
            }
        };
        // Coerce to the lvalue's scalar type on store (e.g. `I64 w; w = 3.14;`
        // truncates), so the interpreter matches the native backend.
        let newv = match target.ty() {
            Some(t) => coerce_to(&t, newv),
            None => newv,
        };
        place.store(newv, pos)
    }

    fn eval_call(
        &mut self,
        callee: &Expr,
        args: &[Expr],
        env: &mut Env,
    ) -> Result<Value, BackendError> {
        let argv = args
            .iter()
            .map(|a| self.eval(a, env))
            .collect::<Result<Vec<_>, _>>()?;
        let pos = callee.span.pos;
        // A direct call to a named function/builtin, unless a variable shadows it.
        if let ExprKind::Ident(name) = &callee.kind {
            if self.resolve(env, name).is_none() {
                return self.call(name, argv, pos);
            }
        }
        // Otherwise the callee evaluates to a function pointer.
        match self.eval(callee, env)? {
            Value::Func(name) => self.call(&name, argv, pos),
            _ => Err(BackendError::at(pos, "called value is not a function")),
        }
    }

    fn call(&mut self, name: &str, args: Vec<Value>, pos: Pos) -> Result<Value, BackendError> {
        if let Some(f) = self.funcs.get(name).cloned() {
            let mut env = Env::func();
            for (i, p) in f.params.iter().enumerate() {
                let val = if i < args.len() {
                    args[i].clone()
                } else if let Some(d) = &p.default {
                    self.eval(d, &mut env)?
                } else {
                    return Err(BackendError::at(
                        pos,
                        format!("missing argument for `{name}`"),
                    ));
                };
                if let Some(pname) = &p.name {
                    // Coerce the argument to the parameter's scalar type, so a
                    // narrow param truncates its argument like the native backend
                    // (which spills the arg at the param's width).
                    let val = coerce_to(&p.ty, by_value(val));
                    env.scopes[0].insert(pname.clone(), cell(val));
                }
            }
            let body = f.body.as_ref().ok_or_else(|| {
                BackendError::at(pos, format!("call to `{name}`, which has no body"))
            })?;
            // Narrow the result to the declared return width (C truncates the
            // return value to the return type), matching the native backend.
            let result = self.exec_func_body(body, &mut env)?;
            return Ok(coerce_to(&f.ret, result));
        }

        // Intrinsics (signatures are registered for type-checking in
        // `sema::seed_builtin_funcs`; the registry lives in `crate::builtins`).
        if crate::builtins::is_builtin(name) {
            return self.call_builtin(name, &args, pos);
        }
        Err(BackendError::at(
            pos,
            format!("call to unknown function `{name}` (external functions are not supported)"),
        ))
    }

    fn call_builtin(
        &mut self,
        name: &str,
        args: &[Value],
        pos: Pos,
    ) -> Result<Value, BackendError> {
        match name {
            "Print" => {
                self.print_formatted(args, pos)?;
                Ok(Value::Void)
            }
            "StrPrint" | "CatPrint" => {
                // Format `fmt, rest...`; `StrPrint` writes at the start of dst,
                // `CatPrint` appends (writes at dst + StrLen(dst)). Both
                // NUL-terminate and return dst.
                let fmt = match &args[1] {
                    Value::Str(s) => s.to_string(),
                    other => String::from_utf8_lossy(&self.cstr_bytes(other, pos)?).into_owned(),
                };
                let s = self.format(&fmt, &args[2..], pos)?;
                let mut bytes = s.into_bytes();
                bytes.push(0);
                let at = if name == "CatPrint" {
                    let cur = self.cstr_bytes(&args[0], pos)?.len() as i64;
                    match &args[0] {
                        Value::Ptr(Some(pv)) => Value::Ptr(Some(pv.offset(cur))),
                        other => other.clone(),
                    }
                } else {
                    args[0].clone()
                };
                self.write_bytes(&at, &bytes, pos)?;
                Ok(args[0].clone())
            }
            "Abs" => Ok(Value::Int(self.to_i64(args[0].clone(), pos)?.abs())),
            "StrLen" => Ok(Value::Int(self.cstr_bytes(&args[0], pos)?.len() as i64)),
            "StrCmp" => {
                let a = self.cstr_bytes(&args[0], pos)?;
                let b = self.cstr_bytes(&args[1], pos)?;
                // Normalized to a sign so the result matches the native backend
                // (libc strcmp's magnitude is unspecified).
                Ok(Value::Int(match a.cmp(&b) {
                    std::cmp::Ordering::Less => -1,
                    std::cmp::Ordering::Equal => 0,
                    std::cmp::Ordering::Greater => 1,
                }))
            }
            "StrCpy" => {
                // Copy src's bytes plus the NUL terminator into dst; return dst.
                let mut bytes = self.cstr_bytes(&args[1], pos)?;
                bytes.push(0);
                self.write_bytes(&args[0], &bytes, pos)?;
                Ok(args[0].clone())
            }
            "MAlloc" => {
                // A bare allocation with no element-type context: a raw byte
                // buffer (a typed `T *p = MAlloc(...)` is retyped by
                // `try_typed_malloc`).
                let n = self.to_i64(args[0].clone(), pos)?.max(0);
                let mut env = Env::top_level();
                self.alloc(n, &Type::U8, &mut env)
            }
            "MStrPrint" => {
                // Format into a freshly allocated, right-sized buffer; return it.
                let fmt = match &args[0] {
                    Value::Str(s) => s.to_string(),
                    other => String::from_utf8_lossy(&self.cstr_bytes(other, pos)?).into_owned(),
                };
                let s = self.format(&fmt, &args[1..], pos)?;
                let mut bytes = s.into_bytes();
                bytes.push(0);
                let mut env = Env::top_level();
                let buf = self.alloc(bytes.len() as i64, &Type::U8, &mut env)?;
                self.write_bytes(&buf, &bytes, pos)?;
                Ok(buf)
            }
            "StrToI64" => {
                let bytes = self.cstr_bytes(&args[0], pos)?;
                Ok(Value::Int(parse_atoll(&bytes)))
            }
            "StrToF64" => {
                let bytes = self.cstr_bytes(&args[0], pos)?;
                Ok(Value::Float(parse_atof(&bytes)))
            }
            "StrToUpper" | "StrToLower" => {
                // ASCII-case the string in place, then return it.
                let up = name == "StrToUpper";
                let mut bytes = self.cstr_bytes(&args[0], pos)?;
                for b in bytes.iter_mut() {
                    *b = if up {
                        b.to_ascii_uppercase()
                    } else {
                        b.to_ascii_lowercase()
                    };
                }
                bytes.push(0);
                self.write_bytes(&args[0], &bytes, pos)?;
                Ok(args[0].clone())
            }
            "StrRev" => {
                // Reverse the string in place, then return it.
                let mut bytes = self.cstr_bytes(&args[0], pos)?;
                bytes.reverse();
                bytes.push(0);
                self.write_bytes(&args[0], &bytes, pos)?;
                Ok(args[0].clone())
            }
            "MemFind" => {
                // Pointer to the first byte == c in buf[0..n], or NULL.
                let c = self.to_i64(args[1].clone(), pos)? as u8;
                let n = self.to_i64(args[2].clone(), pos)?.max(0) as usize;
                let bytes = self.read_n_bytes(&args[0], n, pos)?;
                match bytes.iter().position(|&b| b == c) {
                    Some(i) => self.ptr_at(&args[0], i),
                    None => Ok(Value::Ptr(None)),
                }
            }
            "MemSearch" => {
                // Pointer to the first occurrence of needle[0..nlen] in
                // hay[0..hlen], or NULL (memmem; an empty needle -> hay start).
                let hlen = self.to_i64(args[1].clone(), pos)?.max(0) as usize;
                let nlen = self.to_i64(args[3].clone(), pos)?.max(0) as usize;
                let hay = self.read_n_bytes(&args[0], hlen, pos)?;
                let needle = self.read_n_bytes(&args[2], nlen, pos)?;
                let idx = if nlen == 0 {
                    Some(0)
                } else if nlen > hay.len() {
                    None
                } else {
                    hay.windows(nlen).position(|w| w == needle.as_slice())
                };
                match idx {
                    Some(i) => self.ptr_at(&args[0], i),
                    None => Ok(Value::Ptr(None)),
                }
            }
            "I64ToStr" | "F64ToStr" => {
                // Format the value into buf with a fixed conversion; return buf.
                let fmt = if name == "I64ToStr" { "%d" } else { "%g" };
                let s = self.format(fmt, std::slice::from_ref(&args[0]), pos)?;
                let mut bytes = s.into_bytes();
                bytes.push(0);
                self.write_bytes(&args[1], &bytes, pos)?;
                Ok(args[1].clone())
            }
            "StrSpn" | "StrCSpn" => {
                // Length of the initial run of str whose chars are in / not in set.
                let s = self.cstr_bytes(&args[0], pos)?;
                let set = self.cstr_bytes(&args[1], pos)?;
                let want_in = name == "StrSpn";
                let n = s
                    .iter()
                    .take_while(|&&b| set.contains(&b) == want_in)
                    .count();
                Ok(Value::Int(n as i64))
            }
            "StrChr" | "StrLastChr" => {
                // Pointer to the first/last `c` in str (the NUL counts, so c==0
                // finds the terminator), or NULL.
                let c = self.to_i64(args[1].clone(), pos)? as u8;
                let bytes = self.cstr_bytes(&args[0], pos)?;
                let idx = if c == 0 {
                    Some(bytes.len()) // the terminating NUL
                } else if name == "StrChr" {
                    bytes.iter().position(|&b| b == c)
                } else {
                    bytes.iter().rposition(|&b| b == c)
                };
                match idx {
                    Some(i) => self.ptr_at(&args[0], i),
                    None => Ok(Value::Ptr(None)),
                }
            }
            // Storage is reclaimed by `Rc`; freeing is a no-op.
            "Free" => Ok(Value::Void),
            "StrCat" => {
                // Append src (plus a NUL) at the end of dst's current string.
                let dst_len = self.cstr_bytes(&args[0], pos)?.len() as i64;
                let mut tail = self.cstr_bytes(&args[1], pos)?;
                tail.push(0);
                match as_pointer(&args[0]) {
                    Some(Some(p)) => {
                        self.write_bytes(&Value::Ptr(Some(p.offset(dst_len))), &tail, pos)?
                    }
                    _ => return Err(BackendError::at(pos, "StrCat destination is not a buffer")),
                }
                Ok(args[0].clone())
            }
            // MemMove is overlap-safe; the interpreter reads all `n` bytes before
            // writing, so it is identical to MemCpy here.
            "MemCpy" | "MemMove" => {
                let n = self.to_i64(args[2].clone(), pos)?.max(0) as usize;
                let bytes = self.read_n_bytes(&args[1], n, pos)?;
                self.write_bytes(&args[0], &bytes, pos)?;
                Ok(args[0].clone())
            }
            "MemSet" => {
                let byte = self.to_i64(args[1].clone(), pos)? as u8;
                let n = self.to_i64(args[2].clone(), pos)?.max(0) as usize;
                self.write_bytes(&args[0], &vec![byte; n], pos)?;
                Ok(args[0].clone())
            }
            "MemCmp" => {
                let n = self.to_i64(args[2].clone(), pos)?.max(0) as usize;
                let a = self.read_n_bytes(&args[0], n, pos)?;
                let b = self.read_n_bytes(&args[1], n, pos)?;
                // Normalized to a sign, matching the native backend.
                Ok(Value::Int(match a.cmp(&b) {
                    std::cmp::Ordering::Less => -1,
                    std::cmp::Ordering::Equal => 0,
                    std::cmp::Ordering::Greater => 1,
                }))
            }
            "StrNCmp" => {
                let n = self.to_i64(args[2].clone(), pos)?.max(0) as usize;
                let mut a = self.cstr_bytes(&args[0], pos)?;
                a.truncate(n);
                let mut b = self.cstr_bytes(&args[1], pos)?;
                b.truncate(n);
                Ok(Value::Int(match a.cmp(&b) {
                    std::cmp::Ordering::Less => -1,
                    std::cmp::Ordering::Equal => 0,
                    std::cmp::Ordering::Greater => 1,
                }))
            }
            "StrNCpy" => {
                // Copy up to n chars from src, NUL-padding to n; return dst.
                let n = self.to_i64(args[2].clone(), pos)?.max(0) as usize;
                let mut bytes = self.cstr_bytes(&args[1], pos)?;
                bytes.truncate(n);
                bytes.resize(n, 0);
                self.write_bytes(&args[0], &bytes, pos)?;
                Ok(args[0].clone())
            }
            "Sign" => Ok(Value::Int(self.to_i64(args[0].clone(), pos)?.signum())),
            "RandU64" => Ok(Value::Int(
                crate::builtins::splitmix64(&mut self.rng_state) as i64
            )),
            "StrFind" => {
                // A pointer to the first occurrence of needle in haystack, or NULL.
                // Arg order matches libc `strstr`: haystack first.
                let haystack = self.cstr_bytes(&args[0], pos)?;
                let needle = self.cstr_bytes(&args[1], pos)?;
                match subslice_index(&haystack, &needle) {
                    None => Ok(Value::Ptr(None)),
                    Some(off) => match as_pointer(&args[0]) {
                        // A real pointer into the haystack buffer.
                        Some(Some(p)) => Ok(Value::Ptr(Some(p.offset(off as i64)))),
                        // A bare string literal has no buffer; hand back a pointer
                        // into a fresh region so found-ness is still correct.
                        _ => {
                            let cells = haystack
                                .iter()
                                .map(|&b| cell(Value::Int(b as i64)))
                                .collect();
                            let region = Region::Array(Rc::new(RefCell::new(cells)));
                            Ok(Value::Ptr(Some(PtrVal {
                                region,
                                index: off as i64,
                            })))
                        }
                    },
                }
            }
            "ToUpper" => {
                let c = self.to_i64(args[0].clone(), pos)?;
                Ok(Value::Int(if (97..=122).contains(&c) { c - 32 } else { c }))
            }
            "ToLower" => {
                let c = self.to_i64(args[0].clone(), pos)?;
                Ok(Value::Int(if (65..=90).contains(&c) { c + 32 } else { c }))
            }
            // F64 math (libm). Values match the native backend because Rust's
            // float methods route to the same system libm.
            "Sin" | "Cos" | "Tan" | "Sqrt" | "Floor" | "Ceil" | "Round" | "Exp" | "Ln" | "ASin"
            | "ACos" | "ATan" | "Log10" | "Fabs" | "Pow" | "ATan2" => {
                let num = |i: usize| {
                    args[i]
                        .as_f64()
                        .ok_or_else(|| BackendError::at(pos, "math builtin expects a number"))
                };
                let a = num(0)?;
                Ok(Value::Float(match name {
                    "Sin" => a.sin(),
                    "Cos" => a.cos(),
                    "Tan" => a.tan(),
                    "Sqrt" => a.sqrt(),
                    "Floor" => a.floor(),
                    "Ceil" => a.ceil(),
                    "Round" => a.round(),
                    "Exp" => a.exp(),
                    "Ln" => a.ln(),
                    "ASin" => a.asin(),
                    "ACos" => a.acos(),
                    "ATan" => a.atan(),
                    "Log10" => a.log10(),
                    "Fabs" => a.abs(),
                    "ATan2" => a.atan2(num(1)?),
                    _ => a.powf(num(1)?), // Pow
                }))
            }
            _ => Err(BackendError::at(
                pos,
                format!("builtin `{name}` is not implemented"),
            )),
        }
    }

    /// The bytes of a NUL-terminated string, up to (not including) the
    /// terminator. Accepts a string literal, a pointer into a byte buffer, or an
    /// array that has decayed to one.
    fn cstr_bytes(&self, s: &Value, pos: Pos) -> Result<Vec<u8>, BackendError> {
        let cell_byte = |c: &Cell| c.borrow().as_i64().map(|v| v as u8);
        match s {
            Value::Str(s) => Ok(s.as_bytes().to_vec()),
            Value::Ptr(None) => Err(BackendError::at(pos, "string operation on a null pointer")),
            // A raw byte heap buffer: read bytes directly up to the NUL.
            Value::Ptr(Some(p)) if matches!(p.region, Region::Heap(_)) => {
                let Region::Heap(buf) = &p.region else {
                    unreachable!()
                };
                let buf = buf.borrow();
                Ok(buf[(p.index.max(0) as usize).min(buf.len())..]
                    .iter()
                    .take_while(|&&b| b != 0)
                    .copied()
                    .collect())
            }
            Value::Ptr(Some(p)) => {
                let mut out = Vec::new();
                let mut i = 0;
                while let Some(c) = p.region.cell_at(p.index + i) {
                    match cell_byte(&c) {
                        Some(0) | None => break,
                        Some(b) => out.push(b),
                    }
                    i += 1;
                }
                Ok(out)
            }
            Value::Array(elems) => Ok(elems
                .borrow()
                .iter()
                .map(|c| cell_byte(c).unwrap_or(0))
                .take_while(|&b| b != 0)
                .collect()),
            _ => Err(BackendError::at(pos, "expected a string or pointer")),
        }
    }

    /// Read exactly `n` raw bytes from the buffer `v` points at (for `MemCpy`),
    /// zero-padding past the end. Accepts a string, a pointer, or an array.
    fn read_n_bytes(&self, v: &Value, n: usize, pos: Pos) -> Result<Vec<u8>, BackendError> {
        let byte_at =
            |get: &dyn Fn(usize) -> Option<u8>| (0..n).map(|i| get(i).unwrap_or(0)).collect();
        match v {
            Value::Str(s) => Ok(byte_at(&|i| s.as_bytes().get(i).copied())),
            Value::Ptr(None) => Err(BackendError::at(pos, "memory read from a null pointer")),
            Value::Ptr(Some(p)) if matches!(p.region, Region::Heap(_)) => {
                let Region::Heap(buf) = &p.region else {
                    unreachable!()
                };
                let buf = buf.borrow();
                let base = p.index.max(0) as usize;
                Ok(byte_at(&|i| buf.get(base + i).copied()))
            }
            Value::Ptr(Some(p)) => Ok(byte_at(&|i| {
                p.region
                    .cell_at(p.index + i as i64)
                    .and_then(|c| c.borrow().as_i64())
                    .map(|b| b as u8)
            })),
            Value::Array(elems) => {
                let elems = elems.borrow();
                Ok(byte_at(&|i| {
                    elems
                        .get(i)
                        .and_then(|c| c.borrow().as_i64())
                        .map(|b| b as u8)
                }))
            }
            _ => Err(BackendError::at(pos, "expected a pointer or buffer")),
        }
    }

    /// Write `bytes` into the buffer `dst` points at (a pointer or array).
    /// A pointer `i` bytes past `base` (a buffer pointer), used by the find
    /// builtins to return a pointer into the searched buffer.
    fn ptr_at(&self, base: &Value, i: usize) -> Result<Value, BackendError> {
        Ok(match base {
            Value::Ptr(Some(pv)) => Value::Ptr(Some(pv.offset(i as i64))),
            other => other.clone(),
        })
    }

    fn write_bytes(&self, dst: &Value, bytes: &[u8], pos: Pos) -> Result<(), BackendError> {
        let put = |c: Option<Cell>, b: u8| -> Result<(), BackendError> {
            match c {
                Some(c) => {
                    *c.borrow_mut() = Value::Int(b as i64);
                    Ok(())
                }
                None => Err(BackendError::at(
                    pos,
                    "string write past the end of the buffer",
                )),
            }
        };
        match dst {
            // A raw byte heap buffer: write bytes directly.
            Value::Ptr(Some(p)) if matches!(p.region, Region::Heap(_)) => {
                let Region::Heap(buf) = &p.region else {
                    unreachable!()
                };
                let mut buf = buf.borrow_mut();
                let base = p.index.max(0) as usize;
                if base + bytes.len() > buf.len() {
                    return Err(BackendError::at(
                        pos,
                        "string write past the end of the buffer",
                    ));
                }
                buf[base..base + bytes.len()].copy_from_slice(bytes);
                Ok(())
            }
            Value::Ptr(Some(p)) => {
                for (i, &b) in bytes.iter().enumerate() {
                    put(p.region.cell_at(p.index + i as i64), b)?;
                }
                Ok(())
            }
            Value::Array(elems) => {
                let elems = elems.borrow();
                for (i, &b) in bytes.iter().enumerate() {
                    put(elems.get(i).cloned(), b)?;
                }
                Ok(())
            }
            Value::Ptr(None) => Err(BackendError::at(pos, "string write to a null pointer")),
            _ => Err(BackendError::at(pos, "string write to a non-buffer")),
        }
    }

    /// Format `args` as `"fmt", rest...` and write the result. Shared by the
    /// implicit-print statement and the `Print` intrinsic.
    fn print_formatted(&mut self, args: &[Value], pos: Pos) -> Result<(), BackendError> {
        if let Some(Value::Str(fmt)) = args.first() {
            let s = self.format(fmt, &args[1..], pos)?;
            write!(self.out, "{s}").map_err(|e| self.io(e))?;
        }
        Ok(())
    }

    // ---- expressions (lvalue) ----

    /// The cell an lvalue expression designates (for assignment, `&`, `++/--`).
    fn eval_lvalue(&mut self, e: &Expr, env: &mut Env) -> Result<Cell, BackendError> {
        let pos = e.span.pos;
        match &e.kind {
            ExprKind::Ident(name) => self
                .resolve(env, name)
                .ok_or_else(|| BackendError::at(pos, format!("undefined variable `{name}`"))),
            ExprKind::Member { base, field, arrow } => {
                self.member_cell(base, field, *arrow, pos, env)
            }
            // `a[i]` and `*p` designate the cell the corresponding address points
            // at.
            ExprKind::Index { .. }
            | ExprKind::Unary {
                op: UnOp::Deref, ..
            } => {
                let place = self.eval_addr(e, env)?;
                place.target(pos)
            }
            _ => Err(BackendError::at(pos, "expression is not assignable")),
        }
    }

    /// Resolve an lvalue to a writable [`Place`]: a value cell, or — when it
    /// indexes/derefs a raw byte heap pointer — a typed slot inside that buffer.
    /// Reads and writes go through this so heap access serializes scalars.
    fn eval_place(&mut self, e: &Expr, env: &mut Env) -> Result<Place, BackendError> {
        let pos = e.span.pos;
        match &e.kind {
            ExprKind::Index { .. }
            | ExprKind::Unary {
                op: UnOp::Deref, ..
            } => {
                let pv = self.eval_addr(e, env)?;
                if let Region::Heap(buf) = &pv.region {
                    Ok(Place::Bytes {
                        buf: buf.clone(),
                        off: pv.index.max(0) as usize,
                        ty: e.ty().unwrap_or(Type::I64),
                    })
                } else {
                    Ok(Place::Cell(pv.target(pos)?))
                }
            }
            // Assigning a scalar union field writes into the shared byte buffer.
            ExprKind::Member { base, field, arrow } => {
                if let Some((buf, off, fty)) = self.union_field(base, field, *arrow, pos, env)? {
                    if is_byte_heap_elem(&fty) {
                        return Ok(Place::Bytes {
                            buf,
                            off: off as usize,
                            ty: fty,
                        });
                    }
                    return Err(BackendError::at(
                        pos,
                        "cannot assign to this union field in the interpreter",
                    ));
                }
                Ok(Place::Cell(self.eval_lvalue(e, env)?))
            }
            _ => Ok(Place::Cell(self.eval_lvalue(e, env)?)),
        }
    }

    /// The address (region + offset) of an lvalue expression — the value of `&e`
    /// and the basis for indexing and dereference.
    fn eval_addr(&mut self, e: &Expr, env: &mut Env) -> Result<PtrVal, BackendError> {
        let pos = e.span.pos;
        match &e.kind {
            // `&union.field` is a pointer into the union's shared byte buffer.
            ExprKind::Member { base, field, arrow } => {
                if let Some((buf, off, _)) = self.union_field(base, field, *arrow, pos, env)? {
                    return Ok(PtrVal {
                        region: Region::Heap(buf),
                        index: off as i64,
                    });
                }
                Ok(PtrVal {
                    region: Region::Scalar(self.eval_lvalue(e, env)?),
                    index: 0,
                })
            }
            ExprKind::Ident(_) => Ok(PtrVal {
                region: Region::Scalar(self.eval_lvalue(e, env)?),
                index: 0,
            }),
            ExprKind::Index { base, index } => {
                let bv = self.eval(base, env)?;
                let i = self.to_i64_eval(index, pos, env)?;
                match as_pointer(&bv) {
                    // A raw byte heap region is byte-addressed, so the index
                    // advances by the element's size; cell regions are
                    // element-addressed (index == element).
                    Some(Some(pv)) => {
                        let step = if matches!(pv.region, Region::Heap(_)) {
                            self.size_of_type(&e.ty().unwrap_or(Type::I64), env)?.max(1)
                        } else {
                            1
                        };
                        Ok(pv.offset(i * step))
                    }
                    Some(None) => Err(BackendError::at(pos, "null pointer dereference")),
                    None => Err(BackendError::at(
                        pos,
                        "cannot index a non-array, non-pointer value",
                    )),
                }
            }
            ExprKind::Unary {
                op: UnOp::Deref,
                expr,
            } => {
                let v = self.eval(expr, env)?;
                match as_pointer(&v) {
                    Some(Some(pv)) => Ok(pv),
                    Some(None) => Err(BackendError::at(pos, "null pointer dereference")),
                    None => Err(BackendError::at(pos, "cannot dereference a non-pointer")),
                }
            }
            _ => Err(BackendError::at(
                pos,
                "cannot take the address of this expression",
            )),
        }
    }

    fn member_cell(
        &mut self,
        base: &Expr,
        field: &str,
        arrow: bool,
        pos: Pos,
        env: &mut Env,
    ) -> Result<Cell, BackendError> {
        // The cell whose value is the class/union being accessed.
        let class_cell = if arrow {
            let v = self.eval(base, env)?;
            deref_to_cell(&v, pos)?
        } else if is_place(base) {
            self.eval_lvalue(base, env)?
        } else {
            // An aggregate rvalue (e.g. a class-returning call): materialise the
            // temporary in a fresh cell so its fields can be read.
            cell(self.eval(base, env)?)
        };
        let sv = class_cell.borrow();
        match &*sv {
            Value::Class(fields) => fields
                .borrow()
                .get(field)
                .cloned()
                .ok_or_else(|| BackendError::at(pos, format!("no field `{field}`"))),
            _ => Err(BackendError::at(
                pos,
                "member access on a value that is not a class or union",
            )),
        }
    }

    /// Resolve `base.field` (or `base->field`) to a union byte buffer + the
    /// field's `(offset, type)`, when the field lives in a union — either because
    /// `base` *is* a union, or because `field` is promoted from an anonymous
    /// union embedded in `base`'s class. Returns `None` for an ordinary class
    /// field (which `member_cell` handles).
    fn union_field(
        &mut self,
        base: &Expr,
        field: &str,
        arrow: bool,
        pos: Pos,
        env: &mut Env,
    ) -> Result<Option<(Rc<RefCell<Vec<u8>>>, u64, Type)>, BackendError> {
        let bv = self.eval(base, env)?;
        let uv = if arrow {
            match as_pointer(&bv) {
                Some(Some(pv)) => pv.target(pos)?.borrow().clone(),
                _ => return Ok(None),
            }
        } else {
            bv
        };
        // The base's class name from its static type (deref for `->`).
        let class = match base.ty() {
            Some(Type::Named(n)) if !arrow => n,
            Some(Type::Ptr(inner)) if arrow => match *inner {
                Type::Named(n) => n,
                _ => return Ok(None),
            },
            _ => return Ok(None),
        };
        match uv {
            // The base is itself a union: the field is a typed view into it.
            Value::Union(buf) => {
                let (off, ty) = self.union_member_layout(&class, field, pos)?;
                Ok(Some((buf, off, ty)))
            }
            // The base is a class: the field may be promoted from an anonymous
            // embedded union. A direct field is left to `member_cell`.
            Value::Class(fields) => {
                let direct = self
                    .classes
                    .get(&class)
                    .map(|d| d.fields.iter().any(|f| f.name == field))
                    .unwrap_or(false);
                if direct {
                    return Ok(None);
                }
                let Some((anon, union_class)) = self.find_promoting_anon(&class, field) else {
                    return Ok(None);
                };
                let buf = match fields.borrow().get(&anon).map(|c| c.borrow().clone()) {
                    Some(Value::Union(b)) => b,
                    _ => return Ok(None),
                };
                let (off, ty) = self.union_member_layout(&union_class, field, pos)?;
                Ok(Some((buf, off, ty)))
            }
            _ => Ok(None),
        }
    }

    /// The `(offset, type)` of `field` within the union `class`, from the layout.
    fn union_member_layout(
        &self,
        class: &str,
        field: &str,
        pos: Pos,
    ) -> Result<(u64, Type), BackendError> {
        self.layouts
            .get(class)
            .and_then(|l| l.fields.iter().find(|f| f.name == field))
            .map(|f| (f.offset, f.ty.clone()))
            .ok_or_else(|| BackendError::at(pos, format!("no field `{field}`")))
    }

    /// Find the anonymous embedded-union field of `class` that promotes `field`,
    /// returning `(anon field name, union type name)`.
    fn find_promoting_anon(&self, class: &str, field: &str) -> Option<(String, String)> {
        let def = self.classes.get(class)?;
        for f in &def.fields {
            if crate::sema::is_anon_field(&f.name) {
                if let Type::Named(inner) = &f.ty {
                    if self
                        .classes
                        .get(inner)
                        .map(|idef| idef.fields.iter().any(|m| m.name == field))
                        .unwrap_or(false)
                    {
                        return Some((f.name.clone(), inner.clone()));
                    }
                }
            }
        }
        None
    }

    /// Read a union field from its shared buffer: a scalar deserializes from
    /// bytes; an array field decays to a pointer into the buffer.
    fn read_union_field(
        &self,
        buf: Rc<RefCell<Vec<u8>>>,
        off: u64,
        fty: &Type,
        pos: Pos,
    ) -> Result<Value, BackendError> {
        if is_byte_heap_elem(fty) {
            Place::Bytes {
                buf,
                off: off as usize,
                ty: fty.clone(),
            }
            .load(pos)
        } else if matches!(fty, Type::Array(..)) {
            Ok(Value::Ptr(Some(PtrVal {
                region: Region::Heap(buf),
                index: off as i64,
            })))
        } else {
            Err(BackendError::at(
                pos,
                "this union field type is not supported in the interpreter",
            ))
        }
    }

    // ---- defaults & helpers ----

    /// The size in bytes of a type. Array dimensions are evaluated at runtime,
    /// so `sizeof(arr)` agrees with the array the interpreter actually allocated
    /// (which also evaluates the dimension at runtime). Scalar/class sizes come
    /// from the static layout pass.
    fn size_of_type(&mut self, ty: &Type, env: &mut Env) -> Result<i64, BackendError> {
        match ty {
            Type::Array(elem, Some(dim)) => {
                let count = self.to_i64_eval(dim, dim.span.pos, env)?.max(0);
                let stride = self.layouts.stride_of(elem) as i64;
                Ok(stride * count)
            }
            Type::Array(_, None) => Ok(0),
            _ => Ok(self.layouts.size_of(ty) as i64),
        }
    }

    /// The amount to advance a pointer's `index` by per element it is stepped:
    /// a raw byte-heap pointer steps by the element's byte size; a cell-backed
    /// pointer steps by 1 (cells are element-granular). The element type comes
    /// from `ptr_expr`'s static pointer type.
    fn ptr_step(
        &mut self,
        pv: &PtrVal,
        ptr_expr: &Expr,
        env: &mut Env,
    ) -> Result<i64, BackendError> {
        if !matches!(pv.region, Region::Heap(_)) {
            return Ok(1);
        }
        match ptr_expr.ty().as_ref().and_then(elem_of) {
            Some(elem) => Ok(self.size_of_type(&elem, env)?.max(1)),
            None => Ok(1),
        }
    }

    /// Step a number or pointer by `delta` (for `++`/`--`), scaling a heap
    /// pointer by its element size.
    fn step_value(
        &mut self,
        v: &Value,
        delta: i64,
        expr: &Expr,
        env: &mut Env,
        pos: Pos,
    ) -> Result<Value, BackendError> {
        match v {
            Value::Ptr(Some(pv)) => {
                let step = self.ptr_step(pv, expr, env)?;
                Ok(Value::Ptr(Some(pv.offset(delta * step))))
            }
            _ => step_number(v, delta, pos),
        }
    }

    /// Pointer `+`/`-` where a heap (byte-addressed) pointer is involved, scaled
    /// by the element size. Returns `None` so cell-pointer/number arithmetic
    /// falls through to `apply_binop`.
    fn heap_ptr_arith(
        &mut self,
        op: BinOp,
        l: &Value,
        lhs: &Expr,
        r: &Value,
        rhs: &Expr,
        env: &mut Env,
    ) -> Result<Option<Value>, BackendError> {
        let heap = |v: &Value| match v {
            Value::Ptr(Some(pv)) if matches!(pv.region, Region::Heap(_)) => Some(pv.clone()),
            _ => None,
        };
        match op {
            BinOp::Add => {
                if let (Some(pv), Some(n)) = (heap(l), r.as_i64()) {
                    let s = self.ptr_step(&pv, lhs, env)?;
                    return Ok(Some(Value::Ptr(Some(pv.offset(n * s)))));
                }
                if let (Some(pv), Some(n)) = (heap(r), l.as_i64()) {
                    let s = self.ptr_step(&pv, rhs, env)?;
                    return Ok(Some(Value::Ptr(Some(pv.offset(n * s)))));
                }
                Ok(None)
            }
            BinOp::Sub => {
                if let (Some(a), Some(b)) = (heap(l), heap(r)) {
                    if a.region.same(&b.region) {
                        let s = self.ptr_step(&a, lhs, env)?.max(1);
                        return Ok(Some(Value::Int((a.index - b.index) / s)));
                    }
                }
                if let (Some(pv), Some(n)) = (heap(l), r.as_i64()) {
                    let s = self.ptr_step(&pv, lhs, env)?;
                    return Ok(Some(Value::Ptr(Some(pv.offset(-(n * s))))));
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    /// Build the zero value for a type (for an uninitialised declaration).
    /// Evaluate an initialiser against its declared type. A brace `InitList`
    /// builds an array or class element-by-element (unspecified trailing
    /// elements/fields take their default); any other expression is evaluated
    /// and copied by value, exactly as a plain `=` initialiser.
    fn eval_init(&mut self, init: &Expr, ty: &Type, env: &mut Env) -> Result<Value, BackendError> {
        if let ExprKind::DesignatedInit(items) = &init.kind {
            return self.eval_designated_init(init, items, ty, env);
        }
        let ExprKind::InitList(items) = &init.kind else {
            // `T *p = MAlloc(...)` allocates element-typed cells, matching how the
            // native heap is laid out (so a `Pt *` buffer holds class elements).
            if let Type::Ptr(elem) = ty {
                if let Some(v) = self.try_typed_malloc(init, elem, env)? {
                    return Ok(v);
                }
            }
            return Ok(coerce_to(ty, by_value(self.eval(init, env)?)));
        };
        match ty {
            Type::Array(elem, dim) => {
                let n = dim
                    .as_ref()
                    .map(|d| {
                        self.to_i64_eval(d, d.span.pos, env)
                            .map(|n| n.max(0) as usize)
                    })
                    .transpose()?
                    .unwrap_or(items.len());
                let mut v = Vec::with_capacity(n);
                for i in 0..n {
                    let val = if i < items.len() {
                        self.eval_init(&items[i], elem, env)?
                    } else {
                        self.default_value(elem, env)?
                    };
                    v.push(cell(val));
                }
                Ok(Value::Array(Rc::new(RefCell::new(v))))
            }
            Type::Named(class) => {
                let base = self.default_value(ty, env)?;
                let order = self.field_order(class);
                match &base {
                    Value::Class(fields) => {
                        for (item, (fname, fty, _)) in items.iter().zip(order.iter()) {
                            let val = self.eval_init(item, fty, env)?;
                            if let Some(c) = fields.borrow().get(fname).cloned() {
                                *c.borrow_mut() = val;
                            }
                        }
                    }
                    // A union's brace initializer sets its first member.
                    Value::Union(buf) => {
                        if let (Some(item), Some((_, fty, off))) = (items.first(), order.first()) {
                            self.init_union_field(buf, *off, fty, item, env, init.span.pos)?;
                        }
                    }
                    _ => {}
                }
                Ok(base)
            }
            _ => Err(BackendError::at(
                init.span.pos,
                "an initializer list can only initialize an array, class, or union",
            )),
        }
    }

    /// Evaluate a designated initialiser `{.field = value, ...}` against a
    /// class type. The class starts at its default (zero) value; each named
    /// field is then overwritten with its evaluated initialiser.
    fn eval_designated_init(
        &mut self,
        init: &Expr,
        items: &[(String, Expr)],
        ty: &Type,
        env: &mut Env,
    ) -> Result<Value, BackendError> {
        let Type::Named(class) = ty else {
            return Err(BackendError::at(
                init.span.pos,
                "a designated initializer can only initialize a class or union",
            ));
        };
        let base = self.default_value(ty, env)?;
        // Field name -> (type, offset), captured before the eval loop.
        let order = self.field_order(class);
        let find = |name: &str| order.iter().find(|(n, _, _)| n == name).cloned();
        match &base {
            Value::Class(fields) => {
                for (name, value) in items {
                    let Some((_, fty, _)) = find(name) else {
                        return Err(BackendError::at(
                            value.span.pos,
                            format!("`{class}` has no field `{name}`"),
                        ));
                    };
                    let val = self.eval_init(value, &fty, env)?;
                    if let Some(c) = fields.borrow().get(name).cloned() {
                        *c.borrow_mut() = val;
                    }
                }
            }
            Value::Union(buf) => {
                for (name, value) in items {
                    let Some((_, fty, off)) = find(name) else {
                        return Err(BackendError::at(
                            value.span.pos,
                            format!("`{class}` has no field `{name}`"),
                        ));
                    };
                    self.init_union_field(buf, off, &fty, value, env, value.span.pos)?;
                }
            }
            _ => {}
        }
        Ok(base)
    }

    /// A class/union's fields as `(name, type, byte offset)` in layout order.
    fn field_order(&self, class: &str) -> Vec<(String, Type, u64)> {
        self.layouts
            .get(class)
            .map(|l| {
                l.fields
                    .iter()
                    .map(|f| (f.name.clone(), f.ty.clone(), f.offset))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Initialize a scalar union field by serializing `value` into the shared
    /// buffer at `off`. Non-scalar fields (a rare union brace target) are left
    /// at their zero default.
    fn init_union_field(
        &mut self,
        buf: &Rc<RefCell<Vec<u8>>>,
        off: u64,
        fty: &Type,
        value: &Expr,
        env: &mut Env,
        pos: Pos,
    ) -> Result<(), BackendError> {
        if is_byte_heap_elem(fty) {
            let v = self.eval(value, env)?;
            Place::Bytes {
                buf: buf.clone(),
                off: off as usize,
                ty: fty.clone(),
            }
            .store(v, pos)?;
        }
        Ok(())
    }

    fn default_value(&mut self, ty: &Type, env: &mut Env) -> Result<Value, BackendError> {
        Ok(match ty {
            Type::U0 => Value::Void,
            Type::F64 => Value::Float(0.0),
            Type::Ptr(_) => Value::Ptr(None),
            Type::Named(name) => self.default_class(name)?,
            Type::Array(elem, dim) => {
                let n = match dim {
                    Some(d) => self.to_i64_eval(d, d.span.pos, env)?.max(0) as usize,
                    None => 0,
                };
                let mut v = Vec::with_capacity(n);
                for _ in 0..n {
                    v.push(cell(self.default_value(elem, env)?));
                }
                Value::Array(Rc::new(RefCell::new(v)))
            }
            // All integer / Bool types start at 0.
            _ => Value::Int(0),
        })
    }

    fn default_class(&mut self, name: &str) -> Result<Value, BackendError> {
        let Some(def) = self.classes.get(name).cloned() else {
            return Err(BackendError::new(format!("unknown type `{name}`"), None));
        };
        // A union is a single shared byte buffer the size of its largest field.
        if def.is_union {
            let size = self.layouts.get(name).map(|l| l.size as usize).unwrap_or(0);
            return Ok(Value::Union(Rc::new(RefCell::new(vec![0u8; size]))));
        }
        let mut fields = HashMap::new();
        // Include inherited fields by walking the base chain.
        let mut chain = vec![def];
        let mut cur_base = chain[0].base.clone();
        while let Some(b) = cur_base {
            if let Some(bd) = self.classes.get(&b).cloned() {
                cur_base = bd.base.clone();
                chain.push(bd);
            } else {
                break;
            }
        }
        // Base fields first so derived names can shadow if needed.
        for def in chain.iter().rev() {
            for f in &def.fields {
                let mut env = Env::top_level();
                let v = self.default_value(&f.ty, &mut env)?;
                fields.insert(f.name.clone(), cell(v));
            }
        }
        Ok(Value::Class(Rc::new(RefCell::new(fields))))
    }

    fn num_err(&self, pos: Pos) -> BackendError {
        BackendError::at(pos, "operand is not a number")
    }

    fn to_i64(&self, v: Value, pos: Pos) -> Result<i64, BackendError> {
        v.as_i64()
            .ok_or_else(|| BackendError::at(pos, "expected an integer value"))
    }

    fn to_i64_eval(&mut self, e: &Expr, pos: Pos, env: &mut Env) -> Result<i64, BackendError> {
        let v = self.eval(e, env)?;
        self.to_i64(v, pos)
    }

    /// printf-style formatting supporting `%d %u %x %X %c %s %f %p %%`.
    fn format(&self, fmt: &str, args: &[Value], pos: Pos) -> Result<String, BackendError> {
        use crate::fmt::{render_int, render_str};
        let mut out = String::new();
        let mut chars = fmt.chars().peekable();
        let mut ai = 0;
        // Fetch the next argument for a conversion specifier.
        let take = |ai: &mut usize| -> Result<Value, BackendError> {
            let v = args
                .get(*ai)
                .cloned()
                .ok_or_else(|| BackendError::at(pos, "not enough arguments for format string"))?;
            *ai += 1;
            Ok(v)
        };
        while let Some(c) = chars.next() {
            if c != '%' {
                out.push(c);
                continue;
            }
            let mut spec = crate::fmt::parse(&mut chars);
            if spec.conv == '%' {
                out.push('%');
                continue;
            }
            // `*` width / precision come from arguments, consumed left to right
            // before the value. A negative `*` width means left-justify.
            let width = if spec.width_star {
                let w = value_as_i64(&take(&mut ai)?);
                if w < 0 {
                    spec.minus = true;
                    Some(w.unsigned_abs() as usize)
                } else {
                    Some(w as usize)
                }
            } else {
                spec.width
            };
            let precision = if spec.prec_star {
                let p = value_as_i64(&take(&mut ai)?);
                (p >= 0).then_some(p as usize) // negative precision is omitted
            } else if spec.has_precision {
                Some(spec.precision)
            } else {
                None
            };

            match spec.conv {
                'd' | 'i' => {
                    let n = value_as_i64(&take(&mut ai)?);
                    let sign = if n < 0 {
                        "-"
                    } else if spec.plus {
                        "+"
                    } else if spec.space {
                        " "
                    } else {
                        ""
                    };
                    let digits = n.unsigned_abs().to_string();
                    out.push_str(&render_int(&spec, width, precision, sign, "", &digits));
                }
                'u' => {
                    let u = value_as_i64(&take(&mut ai)?) as u64;
                    out.push_str(&render_int(&spec, width, precision, "", "", &u.to_string()));
                }
                'x' | 'X' => {
                    let u = value_as_i64(&take(&mut ai)?) as u64;
                    let digits = if spec.conv == 'x' {
                        format!("{u:x}")
                    } else {
                        format!("{u:X}")
                    };
                    let alt = match (spec.hash && u != 0, spec.conv) {
                        (true, 'x') => "0x",
                        (true, _) => "0X",
                        _ => "",
                    };
                    out.push_str(&render_int(&spec, width, precision, "", alt, &digits));
                }
                'o' => {
                    let u = value_as_i64(&take(&mut ai)?) as u64;
                    let mut digits = format!("{u:o}");
                    if spec.hash && !digits.starts_with('0') {
                        digits.insert(0, '0');
                    }
                    out.push_str(&render_int(&spec, width, precision, "", "", &digits));
                }
                'c' => {
                    // libc `%c` takes the low byte of the argument.
                    let ch = value_as_i64(&take(&mut ai)?) as u8 as char;
                    out.push_str(&render_str(&spec, width, None, &ch.to_string()));
                }
                's' => {
                    let v = take(&mut ai)?;
                    let body = match &v {
                        Value::Str(s) => s.to_string(),
                        // A pointer/array into a byte buffer: read its bytes up to
                        // the NUL terminator (as `%s` does in C).
                        Value::Ptr(Some(_)) | Value::Array(_) => {
                            String::from_utf8_lossy(&self.cstr_bytes(&v, pos)?).into_owned()
                        }
                        Value::Ptr(None) => "(null)".to_string(),
                        other => format!("{other:?}"),
                    };
                    out.push_str(&render_str(&spec, width, precision, &body));
                }
                'f' | 'e' | 'E' | 'g' | 'G' => {
                    let v = value_as_f64(&take(&mut ai)?);
                    let neg = v.is_sign_negative() && v != 0.0;
                    let mag = v.abs();
                    let body = match spec.conv {
                        // C's default `%f` precision is 6 — match libc exactly.
                        'f' => format!("{mag:.*}", precision.unwrap_or(6)),
                        'e' | 'E' => {
                            crate::fmt::render_exp(mag, precision.unwrap_or(6), spec.conv == 'E')
                        }
                        _ => crate::fmt::render_g(
                            mag,
                            precision.unwrap_or(6),
                            spec.conv == 'G',
                            spec.hash,
                        ),
                    };
                    let sign = float_sign(neg, &spec);
                    out.push_str(&render_int(&spec, width, None, sign, "", &body));
                }
                'p' => {
                    // Addresses can't match the native backend; keep a stable form.
                    let body = match take(&mut ai)? {
                        Value::Ptr(None) => "0x0".to_string(),
                        Value::Ptr(Some(_)) => "0x(ptr)".to_string(),
                        other => format!("{other:?}"),
                    };
                    out.push_str(&render_str(&spec, width, None, &body));
                }
                '\0' => out.push('%'),
                other => {
                    out.push('%');
                    out.push(other);
                }
            }
        }
        Ok(out)
    }
}

impl<W: Write> Backend for Interpreter<W> {
    fn name(&self) -> &'static str {
        "interp"
    }

    /// Run a program. The program must already have passed semantic analysis —
    /// the interpreter relies on the typed AST it produces (e.g. for
    /// `sizeof(expr)`). Use [`run_to_string`] or run [`crate::sema::check_program`]
    /// first.
    fn run(&mut self, program: &Program) -> Result<(), BackendError> {
        self.run_program(program)
    }
}

/// Type-check and run a program, returning everything it printed. This is the
/// safe "compile and run" entry point: it runs semantic analysis first (so the
/// typed AST the interpreter needs is in place) and reports the first semantic
/// error if any.
pub fn run_to_string(program: &Program) -> Result<String, BackendError> {
    if let Some(e) = crate::sema::check_program(program).into_iter().next() {
        return Err(BackendError::at(
            e.pos,
            format!("semantic error: {}", e.message),
        ));
    }
    let mut interp = Interpreter::new(Vec::<u8>::new());
    interp.run(program)?;
    Ok(String::from_utf8_lossy(&interp.into_output()).into_owned())
}

// ---- free helpers ----

fn label_index(stmts: &[Stmt], label: &str) -> Option<usize> {
    stmts
        .iter()
        .position(|s| matches!(&s.kind, StmtKind::Label(l) if l == label))
}

fn compound_binop(op: AssignOp) -> BinOp {
    match op {
        AssignOp::Add => BinOp::Add,
        AssignOp::Sub => BinOp::Sub,
        AssignOp::Mul => BinOp::Mul,
        AssignOp::Div => BinOp::Div,
        AssignOp::Mod => BinOp::Mod,
        AssignOp::BitAnd => BinOp::BitAnd,
        AssignOp::BitOr => BinOp::BitOr,
        AssignOp::BitXor => BinOp::BitXor,
        AssignOp::Shl => BinOp::Shl,
        AssignOp::Shr => BinOp::Shr,
        AssignOp::Assign => unreachable!("plain assignment has no binary op"),
    }
}

/// Add `delta` to a number or advance a pointer (for `++`/`--`).
fn step_number(v: &Value, delta: i64, pos: Pos) -> Result<Value, BackendError> {
    match v {
        Value::Int(i) => Ok(Value::Int(i.wrapping_add(delta))),
        Value::Float(f) => Ok(Value::Float(f + delta as f64)),
        Value::Ptr(Some(pv)) => Ok(Value::Ptr(Some(pv.offset(delta)))),
        _ => Err(BackendError::at(
            pos,
            "`++`/`--` requires a number or pointer",
        )),
    }
}

/// View a value as a pointer, decaying an array to a pointer at index 0.
/// `None` => not pointer-like; `Some(None)` => null; `Some(Some(pv))` => pointer.
fn as_pointer(v: &Value) -> Option<Option<PtrVal>> {
    match v {
        Value::Ptr(p) => Some(p.clone()),
        Value::Array(rc) => Some(Some(PtrVal {
            region: Region::Array(rc.clone()),
            index: 0,
        })),
        // A string literal decays to a pointer to its bytes (NUL-terminated), so
        // `s[i]`, `*s`, and `s + n` work as they would on a `U8*` natively.
        Value::Str(s) => {
            let mut bytes = s.as_bytes().to_vec();
            bytes.push(0);
            Some(Some(PtrVal {
                region: Region::Heap(Rc::new(RefCell::new(bytes))),
                index: 0,
            }))
        }
        _ => None,
    }
}

/// The byte size of a scalar type (for heap byte-buffer access). Pointers and
/// anything non-scalar are 8 bytes.
fn scalar_byte_size(ty: &Type) -> usize {
    match ty {
        Type::I8 | Type::U8 | Type::Bool => 1,
        Type::I16 | Type::U16 => 2,
        Type::I32 | Type::U32 => 4,
        _ => 8,
    }
}

/// Whether a scalar type is signed (so a narrow heap read sign-extends).
fn is_signed_scalar(ty: &Type) -> bool {
    matches!(ty, Type::I8 | Type::I16 | Type::I32 | Type::I64)
}

/// The byte index of the first occurrence of `needle` in `haystack` (an empty
/// needle matches at 0), or `None`. The basis for `StrFind`/`strstr`.
fn subslice_index(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// The element type a pointer or array type points at.
fn elem_of(ty: &Type) -> Option<Type> {
    match ty {
        Type::Ptr(e) | Type::Array(e, _) => Some((**e).clone()),
        _ => None,
    }
}

/// Whether a type is an integer/float scalar that a raw byte heap buffer can
/// hold (so `MAlloc` of it supports type-punning). Pointers/structs/arrays use
/// cell-backed regions instead.
fn is_byte_heap_elem(ty: &Type) -> bool {
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
            | Type::F64
            | Type::Bool
    )
}

/// Sort/equality key for a (possibly null) pointer.
fn ptr_key(p: &Option<PtrVal>) -> (usize, i64) {
    match p {
        None => (0, 0),
        Some(pv) => (pv.region.base_addr(), pv.index),
    }
}

/// Resolve a value used as a pointer to the cell it addresses.
fn deref_to_cell(v: &Value, pos: Pos) -> Result<Cell, BackendError> {
    match as_pointer(v) {
        Some(Some(pv)) => pv.target(pos),
        Some(None) => Err(BackendError::at(pos, "null pointer dereference")),
        None => Err(BackendError::at(pos, "cannot dereference a non-pointer")),
    }
}

fn value_as_i64(v: &Value) -> i64 {
    v.as_i64().unwrap_or(0)
}

fn value_as_f64(v: &Value) -> f64 {
    v.as_f64().unwrap_or(0.0)
}

fn values_equal(l: &Value, r: &Value) -> bool {
    match (l, r) {
        (Value::Ptr(a), Value::Ptr(b)) => match (a, b) {
            (None, None) => true,
            (Some(x), Some(y)) => x.region.same(&y.region) && x.index == y.index,
            _ => false,
        },
        // NULL compared against 0.
        (Value::Ptr(None), v) | (v, Value::Ptr(None)) => v.as_i64() == Some(0),
        (Value::Str(a), Value::Str(b)) => a == b,
        // Float operands compare as f64; two integers compare at full width (an
        // f64 compare loses precision past 2^53, disagreeing with the native cmp).
        _ if l.is_float() || r.is_float() => match (l.as_f64(), r.as_f64()) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        },
        _ => match (l.as_i64(), r.as_i64()) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        },
    }
}

/// Parse a base-10 integer like libc `atoll`: skip leading whitespace, an
/// optional sign, then decimal digits, stopping at the first non-digit.
fn parse_atoll(bytes: &[u8]) -> i64 {
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    let neg = match bytes.get(i) {
        Some(b'-') => {
            i += 1;
            true
        }
        Some(b'+') => {
            i += 1;
            false
        }
        _ => false,
    };
    let mut n: i64 = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        n = n.wrapping_mul(10).wrapping_add((bytes[i] - b'0') as i64);
        i += 1;
    }
    if neg { n.wrapping_neg() } else { n }
}

/// Parse a floating value like libc `atof`: skip leading whitespace, then take
/// the longest valid decimal-float prefix (`[+-]?digits[.digits][e[+-]digits]`)
/// and parse it; anything else yields 0.0.
fn parse_atof(bytes: &[u8]) -> f64 {
    let s = match std::str::from_utf8(bytes) {
        Ok(s) => s.trim_start_matches(|c: char| c.is_ascii_whitespace()),
        Err(_) => return 0.0,
    };
    let b = s.as_bytes();
    let mut i = 0;
    if matches!(b.first(), Some(b'+' | b'-')) {
        i += 1;
    }
    let mut saw_digit = false;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
        saw_digit = true;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
            saw_digit = true;
        }
    }
    if !saw_digit {
        return 0.0;
    }
    if i < b.len() && matches!(b[i], b'e' | b'E') {
        let mut j = i + 1;
        if j < b.len() && matches!(b[j], b'+' | b'-') {
            j += 1;
        }
        if j < b.len() && b[j].is_ascii_digit() {
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            i = j;
        }
    }
    s[..i].parse::<f64>().unwrap_or(0.0)
}

/// Whether `ty` is a signed integer type — drives arithmetic vs logical `>>`,
/// signed vs unsigned `/ %`, and signed vs unsigned relational compares.
fn is_signed_int(ty: &Type) -> bool {
    matches!(ty, Type::I8 | Type::I16 | Type::I32 | Type::I64)
}

/// Whether an expression has a signed type (an unannotated expression defaults
/// to signed, matching HolyC's default `I64`).
fn expr_is_signed(e: &Expr) -> bool {
    e.ty().as_ref().is_none_or(is_signed_int)
}

/// The sign prefix for a float conversion: `-` for negatives, else `+`/space per
/// the flags, else empty.
fn float_sign(neg: bool, spec: &crate::fmt::Spec) -> &'static str {
    if neg {
        "-"
    } else if spec.plus {
        "+"
    } else if spec.space {
        " "
    } else {
        ""
    }
}

/// Coerce a scalar value to the type of the lvalue it is being stored into,
/// matching the native backend (which truncates F64→I64, widens I64→F64, and
/// narrows integer widths in registers on store). Non-scalar targets — pointers,
/// classes, arrays — pass the value through untouched.
fn coerce_to(ty: &Type, v: Value) -> Value {
    match ty {
        Type::F64
        | Type::Bool
        | Type::I8
        | Type::U8
        | Type::I16
        | Type::U16
        | Type::I32
        | Type::U32
        | Type::I64
        | Type::U64 => cast_value(ty, v),
        _ => v,
    }
}

/// Cast a value to `ty`. Narrowing integer casts truncate / sign-extend to the
/// target width (HolyC byte/word arithmetic relies on this).
fn cast_value(ty: &Type, v: Value) -> Value {
    // A float converts to a signed integer via `i64` (`fcvtzs`) but to an
    // unsigned one via `u64` (`fcvtzu`) — they differ past `I64::MAX` and for
    // negatives. The bit pattern is then narrowed by the per-width arms below.
    let i = match &v {
        Value::Float(f) if matches!(ty, Type::U8 | Type::U16 | Type::U32 | Type::U64) => {
            *f as u64 as i64
        }
        _ => value_as_i64(&v),
    };
    match ty {
        Type::F64 => Value::Float(value_as_f64(&v)),
        Type::Bool => Value::Int(i64::from(v.truthy())),
        Type::I8 => Value::Int(i as i8 as i64),
        Type::U8 => Value::Int(i & 0xFF),
        Type::I16 => Value::Int(i as i16 as i64),
        Type::U16 => Value::Int(i & 0xFFFF),
        Type::I32 => Value::Int(i as i32 as i64),
        Type::U32 => Value::Int(i & 0xFFFF_FFFF),
        Type::I64 | Type::U64 => Value::Int(i),
        // Pointers / aggregates / U0 pass through unchanged.
        _ => v,
    }
}
