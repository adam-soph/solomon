//! A tree-walking interpreter for HolyC.
//!
//! It executes a parsed program directly. Run semantic analysis first; the
//! interpreter assumes a well-formed program and only reports faults it hits at
//! run time (division by zero, null dereference, missing function bodies, …).
//!
//! Value & memory model: rather than a byte-addressable heap, every storage
//! location is a *cell* (`Rc<RefCell<Value>>`). Variables, struct fields, and
//! array elements are cells, so `&x` is a handle to a cell, `*p` reads/writes
//! through it, and `p->field` / `a[i]` resolve to cells. This makes pointers and
//! aliasing behave correctly for the common cases. Known simplifications:
//! whole-struct assignment aliases rather than deep-copies, unions don't share
//! storage, and pointer arithmetic beyond `p[0]` is unsupported.
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

/// Recursively copy a struct/array value so it gets independent storage.
fn deep_copy(v: &Value) -> Value {
    match v {
        Value::Struct(fields) => {
            let src = fields.borrow();
            let mut out = HashMap::with_capacity(src.len());
            for (k, c) in src.iter() {
                out.insert(k.clone(), cell(deep_copy(&c.borrow())));
            }
            Value::Struct(Rc::new(RefCell::new(out)))
        }
        Value::Array(elems) => {
            let src = elems.borrow();
            let out: Vec<Cell> = src.iter().map(|c| cell(deep_copy(&c.borrow()))).collect();
            Value::Array(Rc::new(RefCell::new(out)))
        }
        other => other.clone(),
    }
}

/// Apply value semantics when storing into a variable, parameter, or lvalue:
/// HolyC structs are passed and assigned *by value*, so a struct is deep-copied
/// (which also copies any arrays/structs nested inside it). Top-level arrays stay
/// by-reference — they decay to pointers — so they are not copied here.
fn by_value(v: Value) -> Value {
    match v {
        Value::Struct(_) => deep_copy(&v),
        other => other,
    }
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
}

impl Region {
    /// The cell at `index`, or `None` if out of range for this region.
    fn cell_at(&self, index: i64) -> Option<Cell> {
        match self {
            Region::Scalar(c) => (index == 0).then(|| c.clone()),
            Region::Array(rc) if index >= 0 => rc.borrow().get(index as usize).cloned(),
            Region::Array(_) => None,
        }
    }

    /// Whether two regions are the same underlying storage.
    fn same(&self, other: &Region) -> bool {
        match (self, other) {
            (Region::Scalar(a), Region::Scalar(b)) => Rc::ptr_eq(a, b),
            (Region::Array(a), Region::Array(b)) => Rc::ptr_eq(a, b),
            _ => false,
        }
    }

    /// A stable address, used to order/compare pointers into different regions.
    fn base_addr(&self) -> usize {
        match self {
            Region::Scalar(c) => Rc::as_ptr(c) as *const () as usize,
            Region::Array(rc) => Rc::as_ptr(rc) as *const () as usize,
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
    /// A class/union instance: field name -> cell.
    Struct(Rc<RefCell<HashMap<String, Cell>>>),
    /// An array: a list of element cells.
    Array(Rc<RefCell<Vec<Cell>>>),
}

impl Value {
    fn truthy(&self) -> bool {
        match self {
            Value::Int(i) => *i != 0,
            Value::Float(f) => *f != 0.0,
            Value::Ptr(p) => p.is_some(),
            Value::Str(_) | Value::Struct(_) | Value::Array(_) => true,
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
            Value::Struct(_) => write!(f, "Struct{{..}}"),
            Value::Array(a) => write!(f, "Array[{}]", a.borrow().len()),
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
            | StmtKind::Default => Ok(Flow::Normal),

            StmtKind::Expr(e) => {
                self.exec_expr_stmt(e, env)?;
                Ok(Flow::Normal)
            }

            StmtKind::VarDecl { decls } => {
                for d in decls {
                    let v = match &d.init {
                        Some(init) => by_value(self.eval(init, env)?),
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

    /// Execute a function body and reduce its control flow to a return value.
    fn exec_func_body(&mut self, stmts: &[Stmt], env: &mut Env) -> Result<Value, BackendError> {
        match self.exec_stmts(stmts, env)? {
            Flow::Return(v) => Ok(v),
            Flow::Normal => Ok(Value::Void),
            Flow::Goto(label) => Err(BackendError::new(
                format!("goto to undefined label `{label}`"),
                None,
            )),
            Flow::Break | Flow::Continue => {
                Err(BackendError::new("`break`/`continue` outside of a loop", None))
            }
        }
    }

    fn exec_switch(&mut self, cond: &Expr, body: &Stmt, env: &mut Env) -> Result<Flow, BackendError> {
        let v = self.to_i64_eval(cond, cond.span.pos, env)?;
        let StmtKind::Block(stmts) = &body.kind else {
            // A non-block switch body is unusual; just run it.
            return self.exec_stmt(body, env);
        };

        // Find the matching `case`, or `default`.
        let mut start = None;
        let mut default_idx = None;
        for (i, s) in stmts.iter().enumerate() {
            match &s.kind {
                StmtKind::Case { lo, hi } => {
                    let lo = self.to_i64_eval(lo, lo.span.pos, env)?;
                    let matched = match hi {
                        Some(h) => {
                            let h = self.to_i64_eval(h, h.span.pos, env)?;
                            v >= lo && v <= h
                        }
                        None => v == lo,
                    };
                    if matched {
                        start = Some(i);
                        break;
                    }
                }
                StmtKind::Default => default_idx = Some(i),
                _ => {}
            }
        }

        let Some(mut i) = start.or(default_idx) else {
            return Ok(Flow::Normal);
        };

        // Execute from the matched label, falling through until `break`/end.
        env.scopes.push(Scope::new());
        let mut flow = Flow::Normal;
        while i < stmts.len() {
            match self.exec_stmt(&stmts[i], env)? {
                Flow::Normal => {}
                Flow::Break => break,
                other => {
                    flow = other;
                    break;
                }
            }
            i += 1;
        }
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

            ExprKind::Unary { op, expr } => self.eval_unary(*op, expr, pos, env),
            ExprKind::Postfix { op, expr } => self.eval_postfix(*op, expr, pos, env),
            ExprKind::Binary { op, lhs, rhs } => self.eval_binary(*op, lhs, rhs, pos, env),
            ExprKind::Assign { op, target, value } => self.eval_assign(*op, target, value, pos, env),

            ExprKind::Ternary { cond, then, else_ } => {
                if self.eval(cond, env)?.truthy() {
                    self.eval(then, env)
                } else {
                    self.eval(else_, env)
                }
            }

            ExprKind::Call { callee, args } => self.eval_call(callee, args, env),
            ExprKind::Index { .. } | ExprKind::Member { .. } => {
                let c = self.eval_lvalue(e, env)?;
                let v = c.borrow().clone();
                Ok(v)
            }
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
            UnOp::AddrOf => Ok(Value::Ptr(Some(self.eval_addr(expr, env)?))),
            UnOp::PreInc | UnOp::PreDec => {
                let delta = if matches!(op, UnOp::PreInc) { 1 } else { -1 };
                let c = self.eval_lvalue(expr, env)?;
                let nv = step_number(&c.borrow(), delta, pos)?;
                *c.borrow_mut() = nv.clone();
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
        let c = self.eval_lvalue(expr, env)?;
        let old = c.borrow().clone();
        let nv = step_number(&old, delta, pos)?;
        *c.borrow_mut() = nv;
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
        self.apply_binop(op, l, r, pos)
    }

    fn apply_binop(
        &self,
        op: BinOp,
        l: Value,
        r: Value,
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
                        Div => {
                            if b == 0 {
                                return Err(BackendError::at(pos, "division by zero"));
                            }
                            a.wrapping_div(b)
                        }
                        Mod => {
                            if b == 0 {
                                return Err(BackendError::at(pos, "division by zero"));
                            }
                            a.wrapping_rem(b)
                        }
                        _ => unreachable!(),
                    };
                    Ok(Value::Int(v))
                }
            }
            Eq => Ok(Value::Int(i64::from(values_equal(&l, &r)))),
            Ne => Ok(Value::Int(i64::from(!values_equal(&l, &r)))),
            Lt | Gt | Le | Ge => {
                let a = l.as_f64().ok_or_else(|| self.num_err(pos))?;
                let b = r.as_f64().ok_or_else(|| self.num_err(pos))?;
                let v = match op {
                    Lt => a < b,
                    Gt => a > b,
                    Le => a <= b,
                    Ge => a >= b,
                    _ => unreachable!(),
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
                    Shr => a.wrapping_shr(b as u32),
                    _ => unreachable!(),
                };
                Ok(Value::Int(v))
            }
            And | Or => unreachable!("handled with short-circuiting"),
        }
    }

    fn eval_assign(
        &mut self,
        op: AssignOp,
        target: &Expr,
        value: &Expr,
        pos: Pos,
        env: &mut Env,
    ) -> Result<Value, BackendError> {
        let c = self.eval_lvalue(target, env)?;
        let rhs = self.eval(value, env)?;
        let newv = match op {
            AssignOp::Assign => rhs,
            _ => {
                let cur = c.borrow().clone();
                self.apply_binop(compound_binop(op), cur, rhs, pos)?
            }
        };
        let stored = by_value(newv);
        *c.borrow_mut() = stored.clone();
        Ok(stored)
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
        match &callee.kind {
            ExprKind::Ident(name) => self.call(name, argv, callee.span.pos),
            _ => Err(BackendError::at(
                callee.span.pos,
                "calling a computed value (function pointer) is not supported",
            )),
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
                    env.scopes[0].insert(pname.clone(), cell(by_value(val)));
                }
            }
            let body = f.body.as_ref().ok_or_else(|| {
                BackendError::at(pos, format!("call to `{name}`, which has no body"))
            })?;
            return self.exec_func_body(body, &mut env);
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
            _ => Err(BackendError::at(pos, format!("builtin `{name}` is not implemented"))),
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
            ExprKind::Index { .. } | ExprKind::Unary { op: UnOp::Deref, .. } => {
                let place = self.eval_addr(e, env)?;
                place.target(pos)
            }
            _ => Err(BackendError::at(pos, "expression is not assignable")),
        }
    }

    /// The address (region + offset) of an lvalue expression — the value of `&e`
    /// and the basis for indexing and dereference.
    fn eval_addr(&mut self, e: &Expr, env: &mut Env) -> Result<PtrVal, BackendError> {
        let pos = e.span.pos;
        match &e.kind {
            ExprKind::Ident(_) | ExprKind::Member { .. } => Ok(PtrVal {
                region: Region::Scalar(self.eval_lvalue(e, env)?),
                index: 0,
            }),
            ExprKind::Index { base, index } => {
                let bv = self.eval(base, env)?;
                let i = self.to_i64_eval(index, pos, env)?;
                match as_pointer(&bv) {
                    Some(Some(pv)) => Ok(pv.offset(i)),
                    Some(None) => Err(BackendError::at(pos, "null pointer dereference")),
                    None => Err(BackendError::at(
                        pos,
                        "cannot index a non-array, non-pointer value",
                    )),
                }
            }
            ExprKind::Unary { op: UnOp::Deref, expr } => {
                let v = self.eval(expr, env)?;
                match as_pointer(&v) {
                    Some(Some(pv)) => Ok(pv),
                    Some(None) => Err(BackendError::at(pos, "null pointer dereference")),
                    None => Err(BackendError::at(pos, "cannot dereference a non-pointer")),
                }
            }
            _ => Err(BackendError::at(pos, "cannot take the address of this expression")),
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
        // The cell whose value is the struct/union being accessed.
        let struct_cell = if arrow {
            let v = self.eval(base, env)?;
            deref_to_cell(&v, pos)?
        } else {
            self.eval_lvalue(base, env)?
        };
        let sv = struct_cell.borrow();
        match &*sv {
            Value::Struct(fields) => fields
                .borrow()
                .get(field)
                .cloned()
                .ok_or_else(|| BackendError::at(pos, format!("no field `{field}`"))),
            _ => Err(BackendError::at(pos, "member access on a non-struct value")),
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

    /// Build the zero value for a type (for an uninitialised declaration).
    fn default_value(&mut self, ty: &Type, env: &mut Env) -> Result<Value, BackendError> {
        Ok(match ty {
            Type::U0 => Value::Void,
            Type::F64 => Value::Float(0.0),
            Type::Ptr(_) => Value::Ptr(None),
            Type::Named(name) => self.default_struct(name)?,
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

    fn default_struct(&mut self, name: &str) -> Result<Value, BackendError> {
        let Some(def) = self.classes.get(name).cloned() else {
            return Err(BackendError::new(format!("unknown type `{name}`"), None));
        };
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
        Ok(Value::Struct(Rc::new(RefCell::new(fields))))
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
        let mut out = String::new();
        let mut chars = fmt.chars();
        let mut ai = 0;
        // Fetch the next argument for a conversion specifier.
        let arg = |ai: &mut usize| -> Result<Value, BackendError> {
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
            match chars.next() {
                Some('%') => out.push('%'),
                Some('d') | Some('i') => out.push_str(&value_as_i64(&arg(&mut ai)?).to_string()),
                Some('u') => out.push_str(&(value_as_i64(&arg(&mut ai)?) as u64).to_string()),
                Some('x') => out.push_str(&format!("{:x}", value_as_i64(&arg(&mut ai)?))),
                Some('X') => out.push_str(&format!("{:X}", value_as_i64(&arg(&mut ai)?))),
                Some('c') => {
                    if let Some(ch) = char::from_u32(value_as_i64(&arg(&mut ai)?) as u32) {
                        out.push(ch);
                    }
                }
                Some('s') => match arg(&mut ai)? {
                    Value::Str(s) => out.push_str(&s),
                    other => out.push_str(&format!("{other:?}")),
                },
                Some('f') => out.push_str(&value_as_f64(&arg(&mut ai)?).to_string()),
                Some('p') => match arg(&mut ai)? {
                    Value::Ptr(None) => out.push_str("0x0"),
                    Value::Ptr(Some(_)) => out.push_str("0x(ptr)"),
                    other => out.push_str(&format!("{other:?}")),
                },
                Some(other) => {
                    out.push('%');
                    out.push(other);
                }
                None => out.push('%'),
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
        return Err(BackendError::at(e.pos, format!("semantic error: {}", e.message)));
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
        _ => Err(BackendError::at(pos, "`++`/`--` requires a number or pointer")),
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
        _ => None,
    }
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
        _ => match (l.as_f64(), r.as_f64()) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        },
    }
}

/// Cast a value to `ty`. Narrowing integer casts truncate / sign-extend to the
/// target width (HolyC byte/word arithmetic relies on this).
fn cast_value(ty: &Type, v: Value) -> Value {
    let i = value_as_i64(&v);
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

