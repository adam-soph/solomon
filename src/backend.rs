//! Shared, non-emitting code-generation helpers used by both native backends
//! ([`crate::arm64`] and [`crate::x86_64`]).
//!
//! solomon has no IR. Each backend walks the typed AST and emits machine code
//! directly, which makes it easy for the two to drift on *decisions* that must
//! agree. The sharpest case is the `printf` runtime ABI — the packed flag bits and
//! the conversion→radix mapping. Both backends bake these into their emitted
//! formatters, so they have to be byte-for-byte identical for the
//! interpreter-as-oracle conformance to hold. This module is the single source of
//! truth for those pure decisions.
//!
//! Everything here returns plain data and emits nothing, so it cannot change an
//! output byte on its own. The backends consume the results and do the actual
//! instruction emission.

use std::collections::HashMap;

use crate::ast::{Expr, ExprKind, Stmt, StmtKind, Type};
use crate::codegen::CodegenError;
use crate::layout::Layouts;
use crate::token::Pos;

/// Reports whether `ty` is an aggregate (a class/union or array).
///
/// An aggregate is represented by its address and never lives in a register, so it
/// is passed and copied by reference. Shared by both backends and the
/// [`gen_init_into`] driver.
pub fn is_aggregate(ty: &Type) -> bool {
    matches!(ty, Type::Named(_) | Type::Array(..))
}

/// A backend's leaf-emit hooks for the shared, backend-independent code-generation
/// drivers in this module.
///
/// Those drivers are the initializer lowering ([`gen_init_into`]), the control-flow
/// lowering ([`gen_switch`]/[`gen_if`]/[`gen_while`]/[`gen_do_while`]/[`gen_for`]),
/// and the call lowering ([`gen_call`]). The *structure* of each is identical across
/// targets and lives in the drivers: the recursion, label placement, the
/// `break`/`continue` target stacks, the switch partition and dispatch, the loop
/// back-edges, the call shape. A backend supplies only the leaf emits — machine
/// stores, conditional branches off an evaluated expression, argument marshalling —
/// plus the bits the drivers can't compute themselves.
///
/// This is one TCC-style emitter vtable, not an IR: emission stays immediate. Both
/// native backends ([`crate::arm64`] and [`crate::x86_64`]) implement it on their
/// inner `Cg` worker, so the two can't drift on the shared structure.
pub trait Emitter {
    /// Where the aggregate being initialised lives — a frame slot or a global.
    /// Opaque to [`gen_init_into`], which only forwards it to the leaf stores.
    type Place: Copy;
    /// A frame-slot handle (an offset). Holds the evaluated `switch` value
    /// ([`gen_switch`]) and the sret result ([`gen_call`]); opaque to the drivers.
    type Slot: Copy;

    /// The backend's name for diagnostics, e.g. `"arm64 backend"`.
    fn backend_label(&self) -> &'static str;

    // ---- initializer leaves (driven by [`gen_init_into`]) ----

    /// The shared `repr(C)` layout table (for strides and field offsets).
    fn init_layouts(&self) -> &Layouts;
    /// Store a scalar/pointer leaf `init` of type `ty` at `byte_off` within `place`.
    fn emit_scalar_init(
        &mut self,
        place: Self::Place,
        byte_off: u32,
        ty: &Type,
        init: &Expr,
    ) -> Result<(), CodegenError>;
    /// Store an `F64` leaf at `byte_off` within `place`.
    fn emit_float_init(
        &mut self,
        place: Self::Place,
        byte_off: u32,
        init: &Expr,
    ) -> Result<(), CodegenError>;
    /// Copy an aggregate-valued leaf `init` of type `ty` (by-value class/array).
    fn emit_aggregate_init(
        &mut self,
        place: Self::Place,
        byte_off: u32,
        ty: &Type,
        init: &Expr,
    ) -> Result<(), CodegenError>;

    // ---- control-flow leaves (driven by [`gen_switch`]/[`gen_if`]/the loops) ----

    /// Allocate a fresh branch label id.
    fn new_label(&mut self) -> usize;
    /// Place label `l` at the current position.
    fn place_label(&mut self, l: usize);
    /// Emit an unconditional branch to `l`.
    fn branch(&mut self, l: usize);
    /// Push/pop the `break` target (a loop/switch exit).
    fn push_break(&mut self, l: usize);
    fn pop_break(&mut self);
    /// Push/pop the `continue` target (a loop's continuation point).
    fn push_continue(&mut self, l: usize);
    fn pop_continue(&mut self);
    /// Enter/leave a lexical scope.
    fn enter_scope(&mut self);
    fn exit_scope(&mut self);
    /// Emit one body statement (recurses into the backend's statement lowering).
    fn lower_stmt(&mut self, s: &Stmt) -> Result<(), CodegenError>;
    /// Evaluate `cond` and branch to `label` when it is **false** (zero).
    fn branch_if_false(&mut self, cond: &Expr, label: usize) -> Result<(), CodegenError>;
    /// Evaluate `cond` and branch to `label` when it is **true** (non-zero).
    fn branch_if_true(&mut self, cond: &Expr, label: usize) -> Result<(), CodegenError>;
    /// Evaluate `e` for its side effects, discarding the value (a `for` step).
    fn eval_expr_discard(&mut self, e: &Expr) -> Result<(), CodegenError>;
    /// Evaluate the switch value and store it to a fresh slot, returning the slot.
    fn eval_switch_value(&mut self, cond: &Expr) -> Result<Self::Slot, CodegenError>;
    /// Evaluate `bound`, compare the stored switch value against it, and branch to
    /// `target` when `switch_value <cc> bound`.
    fn switch_cmp_branch(
        &mut self,
        slot: Self::Slot,
        bound: &Expr,
        cc: SwitchCc,
        target: usize,
    ) -> Result<(), CodegenError>;
    /// Tries an O(1) jump table instead of the compare-chain. Returns `Ok(true)` when
    /// it emitted the table, so the driver skips the chain; `Ok(false)` to fall back.
    /// The default is no table (the freestanding/x86 path); arm64 overrides it.
    fn try_switch_table(
        &mut self,
        _stmts: &[Stmt],
        _label_at: &HashMap<usize, usize>,
        _slot: Self::Slot,
        _gap_target: usize,
    ) -> Result<bool, CodegenError> {
        Ok(false)
    }

    // ---- call leaves (driven by [`gen_call`]) ----

    /// Evaluate the indirect-call target and spill it deepest on the stack, so it
    /// survives argument evaluation. The call-instruction closure pops it back.
    fn spill_callee(&mut self, callee: &Expr) -> Result<(), CodegenError>;
    /// Allocate an sret result slot when `ret` is returned by value, else `None`.
    fn alloc_sret(&mut self, ret: &Type) -> Option<Self::Slot>;
    /// Evaluate one named argument of type `ty` and spill its 8 bytes to the stack.
    fn eval_arg_spill(&mut self, ty: &Type, arg: &Expr) -> Result<(), CodegenError>;
    /// Place the arguments in their ABI registers, encapsulating each backend's
    /// marshalling. Stages the trailing variadic `extra` args whenever `varargs` is
    /// set, even with none present (the hidden count is then 0); pops the named args
    /// into their registers, in reverse, per `classes`; and places the hidden variadic
    /// `(ptr, count)` pair.
    fn place_args(
        &mut self,
        classes: &[ArgClass],
        extra: &[&Expr],
        varargs: bool,
        pos: Pos,
    ) -> Result<(), CodegenError>;
    /// Set the sret pointer register from `slot`, just before the call. A `None` slot
    /// (a non-aggregate return) is a no-op.
    fn set_sret_reg(&mut self, slot: Option<Self::Slot>);
    /// Deliver the call result into the expression-evaluation register(s). For an
    /// aggregate return that is the sret temp's address; otherwise the float or
    /// integer result register.
    fn deliver_result(&mut self, ret: &Type, sret: Option<Self::Slot>);
    /// Snapshot the frame allocator's bump pointer so a call's transient scratch (the
    /// variadic buffer) can be reclaimed afterwards. The high-water frame size is
    /// tracked separately, so reclaiming never shrinks the frame.
    fn frame_mark(&self) -> u32;
    /// Reset the bump pointer to a `frame_mark` snapshot, reclaiming the variadic
    /// buffer. The next sequential call reuses it. A *nested* call allocated above
    /// this mark, so the two never overlap.
    fn frame_reset(&mut self, mark: u32);
}

/// Emit the stores for a brace or designated initialiser (or a single leaf value)
/// into the aggregate at `place`, at byte offset `byte_off`.
///
/// Recurses for nested arrays and classes. Only the provided elements and fields are
/// written, so partial initialisers leave the rest zero. This is safe because the
/// caller has already zeroed local slots, and globals are linker-zeroed.
///
/// Backend-independent: the per-leaf machine stores are the backend's via
/// [`Emitter`]. Shared by both native backends so the two can't drift.
pub fn gen_init_into<E: Emitter>(
    cg: &mut E,
    place: E::Place,
    ty: &Type,
    byte_off: u32,
    init: &Expr,
) -> Result<(), CodegenError> {
    if let ExprKind::InitList(items) = &init.kind {
        match ty {
            Type::Array(elem, _) => {
                let stride = cg.init_layouts().stride_of(elem) as u32;
                for (i, item) in items.iter().enumerate() {
                    gen_init_into(cg, place, elem, byte_off + i as u32 * stride, item)?;
                }
            }
            Type::Named(class) => {
                // Collect (type, offset) before the store loop, so the immutable
                // layout borrow ends before the recursive `&mut cg` calls.
                let fields: Vec<(Type, u32)> = cg
                    .init_layouts()
                    .get(class)
                    .map(|l| {
                        l.fields
                            .iter()
                            .map(|f| (f.ty.clone(), f.offset as u32))
                            .collect()
                    })
                    .unwrap_or_default();
                for (item, (fty, foff)) in items.iter().zip(fields.iter()) {
                    gen_init_into(cg, place, fty, byte_off + foff, item)?;
                }
            }
            _ => {
                return Err(CodegenError::at(
                    init.span.pos,
                    format!(
                        "{}: an initializer list can only initialize an array, class, or union",
                        cg.backend_label()
                    ),
                ));
            }
        }
        return Ok(());
    }
    if let ExprKind::DesignatedInit(items) = &init.kind {
        let Type::Named(class) = ty else {
            return Err(CodegenError::at(
                init.span.pos,
                format!(
                    "{}: a designated initializer can only initialize a class or union",
                    cg.backend_label()
                ),
            ));
        };
        let fields: Vec<(String, Type, u32)> = cg
            .init_layouts()
            .get(class)
            .map(|l| {
                l.fields
                    .iter()
                    .map(|f| (f.name.clone(), f.ty.clone(), f.offset as u32))
                    .collect()
            })
            .unwrap_or_default();
        for (name, value) in items {
            let Some((_, fty, foff)) = fields.iter().find(|(n, _, _)| n == name) else {
                return Err(CodegenError::at(
                    value.span.pos,
                    format!("{}: `{class}` has no field `{name}`", cg.backend_label()),
                ));
            };
            let (fty, foff) = (fty.clone(), *foff);
            gen_init_into(cg, place, &fty, byte_off + foff, value)?;
        }
        return Ok(());
    }
    // A leaf value: a float, an aggregate-valued expression, or a scalar/pointer.
    if matches!(ty, Type::F64) {
        cg.emit_float_init(place, byte_off, init)
    } else if is_aggregate(ty) {
        cg.emit_aggregate_init(place, byte_off, ty, init)
    } else {
        cg.emit_scalar_init(place, byte_off, ty, init)
    }
}

/// The relation tested when matching a `switch` value against a case bound.
///
/// Used by [`Emitter::switch_cmp_branch`], which branches when
/// `switch_value <cc> bound`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwitchCc {
    /// `==` — a single `case` value matched.
    Eq,
    /// `<` — below a `lo ... hi` range's lower bound (skip the case).
    Lt,
    /// `>` — above a range's upper bound (skip the case).
    Gt,
}

/// Lower a `switch`.
///
/// The control structure is backend-independent. It evaluates the value to a slot,
/// runs any `start:` prologue, and assigns a label per `case`/`default`. Dispatch is
/// an optional jump table, else a compare-chain to the matching label; a value that
/// matches nothing falls through to `default`, then the `end:` epilogue, then the
/// exit. It then emits the body, placing each label and the epilogue.
///
/// The leaf emits are the backend's via [`Emitter`]. Shared by both native backends
/// so they can't drift on the subtle `start:`/`end:` and gap-target semantics.
pub fn gen_switch<E: Emitter>(
    cg: &mut E,
    cond: &Expr,
    body: &Stmt,
    pos: Pos,
) -> Result<(), CodegenError> {
    let StmtKind::Block(stmts) = &body.kind else {
        return Err(CodegenError::at(
            pos,
            format!("{}: switch body must be a block", cg.backend_label()),
        ));
    };

    let slot = cg.eval_switch_value(cond)?;

    // `start:` / `end:` sub-labels partition the body into an optional prologue and
    // epilogue. The prologue runs on entry, before dispatch. The epilogue is reached
    // by fall-through, and `break` skips it. Sema has checked the ordering.
    let start_idx = stmts
        .iter()
        .position(|s| matches!(s.kind, StmtKind::SwitchStart));
    let first_case = stmts
        .iter()
        .position(|s| matches!(s.kind, StmtKind::Case { .. } | StmtKind::Default));
    let end_idx = stmts
        .iter()
        .position(|s| matches!(s.kind, StmtKind::SwitchEnd));
    let prologue = start_idx.map(|si| (si + 1)..first_case.unwrap_or(stmts.len()));

    let l_end = cg.new_label();
    cg.push_break(l_end);
    cg.enter_scope();

    // Prologue: always runs, before the dispatch compares.
    if let Some(range) = prologue.clone() {
        for st in &stmts[range] {
            cg.lower_stmt(st)?;
        }
    }

    let mut label_at: HashMap<usize, usize> = HashMap::new();
    let mut default_label: Option<usize> = None;
    let end_label = end_idx.map(|_| cg.new_label());
    for (i, st) in stmts.iter().enumerate() {
        match &st.kind {
            StmtKind::Case { .. } => {
                let l = cg.new_label();
                label_at.insert(i, l);
            }
            StmtKind::Default => {
                let l = cg.new_label();
                label_at.insert(i, l);
                default_label = Some(l);
            }
            _ => {}
        }
    }

    // When no case matched, fall to default, else the epilogue, else the exit.
    let gap_target = default_label.or(end_label).unwrap_or(l_end);
    // Prefer an O(1) jump table when the backend offers one; else a compare-chain.
    if !cg.try_switch_table(stmts, &label_at, slot, gap_target)? {
        for (i, st) in stmts.iter().enumerate() {
            if let StmtKind::Case { lo, hi } = &st.kind {
                let target = label_at[&i];
                match hi {
                    None => cg.switch_cmp_branch(slot, lo, SwitchCc::Eq, target)?,
                    Some(hi) => {
                        // lo <= v <= hi: skip when v < lo or v > hi.
                        let skip = cg.new_label();
                        cg.switch_cmp_branch(slot, lo, SwitchCc::Lt, skip)?;
                        cg.switch_cmp_branch(slot, hi, SwitchCc::Gt, skip)?;
                        cg.branch(target);
                        cg.place_label(skip);
                    }
                }
            }
        }
        cg.branch(gap_target);
    }

    // Emit the body, placing each case/default label and the epilogue marker.
    for (i, st) in stmts.iter().enumerate() {
        if prologue.as_ref().is_some_and(|r| r.contains(&i)) {
            continue; // already emitted as the prologue
        }
        if let Some(&l) = label_at.get(&i) {
            cg.place_label(l);
        }
        match &st.kind {
            StmtKind::Case { .. } | StmtKind::Default | StmtKind::SwitchStart => {}
            StmtKind::SwitchEnd => {
                if let Some(l) = end_label {
                    cg.place_label(l);
                }
            }
            _ => cg.lower_stmt(st)?,
        }
    }
    cg.exit_scope();
    cg.pop_break();
    cg.place_label(l_end);
    Ok(())
}

/// Lower an `if` or `if`/`else`.
///
/// Branches past the `then` arm when the condition is false. With an `else`, jumps
/// over it after the `then` arm. Backend-independent: labels, branches, and
/// recursion go through [`Emitter`].
pub fn gen_if<E: Emitter>(
    cg: &mut E,
    cond: &Expr,
    then: &Stmt,
    else_: Option<&Stmt>,
) -> Result<(), CodegenError> {
    let l_else = cg.new_label();
    cg.branch_if_false(cond, l_else)?;
    cg.lower_stmt(then)?;
    if let Some(else_branch) = else_ {
        let l_end = cg.new_label();
        cg.branch(l_end);
        cg.place_label(l_else);
        cg.lower_stmt(else_branch)?;
        cg.place_label(l_end);
    } else {
        cg.place_label(l_else);
    }
    Ok(())
}

/// Lower a `while`: test at the top, exit when false, loop back after the body.
/// `continue` targets the top, so it re-tests the condition.
pub fn gen_while<E: Emitter>(cg: &mut E, cond: &Expr, body: &Stmt) -> Result<(), CodegenError> {
    let l_top = cg.new_label();
    let l_end = cg.new_label();
    cg.place_label(l_top);
    cg.branch_if_false(cond, l_end)?;
    cg.push_break(l_end);
    cg.push_continue(l_top);
    cg.lower_stmt(body)?;
    cg.pop_break();
    cg.pop_continue();
    cg.branch(l_top);
    cg.place_label(l_end);
    Ok(())
}

/// Lower a `do`/`while`: run the body, then test at the bottom and loop back while
/// true. `continue` targets the bottom test (`l_cont`).
pub fn gen_do_while<E: Emitter>(cg: &mut E, body: &Stmt, cond: &Expr) -> Result<(), CodegenError> {
    let l_top = cg.new_label();
    let l_cont = cg.new_label();
    let l_end = cg.new_label();
    cg.place_label(l_top);
    cg.push_break(l_end);
    cg.push_continue(l_cont);
    cg.lower_stmt(body)?;
    cg.pop_break();
    cg.pop_continue();
    cg.place_label(l_cont);
    cg.branch_if_true(cond, l_top)?;
    cg.place_label(l_end);
    Ok(())
}

/// Lower a call.
///
/// The shape is backend-independent: spill an indirect callee, allocate the sret
/// slot, evaluate and spill the named args left-to-right, place all args in registers
/// via [`Emitter::place_args`], set the sret pointer, emit the call instruction via
/// the `emit_call_insn` closure, then deliver the result. Shared by both native
/// backends so the call shape can't drift.
///
/// Two steps stay per-backend. Placing the arguments genuinely differs in
/// register/stack strategy — most sharply, the two backends marshal variadics
/// differently, by direct address computation vs a stack round-trip — so
/// [`Emitter::place_args`] is a hook that keeps each target's exact strategy
/// verbatim. The call instruction itself is the `emit_call_insn` closure, because
/// each backend's `CallTarget` enum differs.
#[allow(clippy::too_many_arguments)]
pub fn gen_call<E: Emitter>(
    cg: &mut E,
    indirect_callee: Option<&Expr>,
    named: &[(&Type, &Expr)],
    extra: &[&Expr],
    classes: &[ArgClass],
    ret: &Type,
    varargs: bool,
    pos: Pos,
    emit_call_insn: impl FnOnce(&mut E),
) -> Result<(), CodegenError> {
    if let Some(callee) = indirect_callee {
        cg.spill_callee(callee)?;
    }
    let sret = cg.alloc_sret(ret);
    for (ty, arg) in named {
        cg.eval_arg_spill(ty, arg)?;
    }
    // The variadic buffer `place_args` stages is dead once the call returns, so
    // reclaim it. Without this, a function with many variadic calls (say hundreds of
    // `Print`s) would grow its frame by one buffer per call and overflow.
    let mark = cg.frame_mark();
    cg.place_args(classes, extra, varargs, pos)?;
    cg.set_sret_reg(sret);
    emit_call_insn(cg);
    cg.frame_reset(mark);
    cg.deliver_result(ret, sret);
    Ok(())
}

/// Lower a `for`: optional init in a fresh scope, top test, body, then the `continue`
/// target at the step, the step, and the loop back. An absent condition loops
/// unconditionally.
pub fn gen_for<E: Emitter>(
    cg: &mut E,
    init: Option<&Stmt>,
    cond: Option<&Expr>,
    step: Option<&Expr>,
    body: &Stmt,
) -> Result<(), CodegenError> {
    cg.enter_scope();
    if let Some(init) = init {
        cg.lower_stmt(init)?;
    }
    let l_top = cg.new_label();
    let l_cont = cg.new_label();
    let l_end = cg.new_label();
    cg.place_label(l_top);
    if let Some(cond) = cond {
        cg.branch_if_false(cond, l_end)?;
    }
    cg.push_break(l_end);
    cg.push_continue(l_cont);
    cg.lower_stmt(body)?;
    cg.pop_break();
    cg.pop_continue();
    cg.place_label(l_cont);
    if let Some(step) = step {
        cg.eval_expr_discard(step)?;
    }
    cg.branch(l_top);
    cg.place_label(l_end);
    cg.exit_scope();
    Ok(())
}

/// Where one argument is passed under the backends' internal calling convention:
/// the `n`-th integer/pointer register, or the `n`-th floating-point register.
///
/// The two classes are numbered independently, as in both AAPCS64 and System V.
/// Register *names* and per-class register *counts* are each backend's concern. This
/// is the shared classification *sequence*, which must agree so that a caller and
/// callee built by different backends still match.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArgClass {
    /// The `n`-th integer/pointer argument register (also carries by-address
    /// aggregates).
    Int(usize),
    /// The `n`-th floating-point argument register.
    Float(usize),
}

/// Which register class ran out in [`classify_args`], so the caller can raise its
/// own target-named diagnostic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArgOverflow {
    Int,
    Float,
}

/// Classify each argument by its parameter type. An `F64` takes the next
/// floating-point register; everything else takes the next integer register,
/// including integers, pointers, and by-address aggregates.
///
/// `max_int` and `max_fp` bound each class (AAPCS64 is 8/8, System V is 6/8); on
/// overflow the offending class is returned. The two counters advance independently,
/// matching both target ABIs.
pub fn classify_args<'a>(
    tys: impl IntoIterator<Item = &'a Type>,
    max_int: usize,
    max_fp: usize,
) -> Result<Vec<ArgClass>, ArgOverflow> {
    let mut int = 0;
    let mut fp = 0;
    let mut out = Vec::new();
    for ty in tys {
        if matches!(ty, Type::F64) {
            if fp >= max_fp {
                return Err(ArgOverflow::Float);
            }
            out.push(ArgClass::Float(fp));
            fp += 1;
        } else {
            if int >= max_int {
                return Err(ArgOverflow::Int);
            }
            out.push(ArgClass::Int(int));
            int += 1;
        }
    }
    Ok(out)
}
