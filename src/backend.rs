//! Shared, **non-emitting** code-generation helpers used by both native backends
//! ([`crate::arm64`] and [`crate::x86_64`]).
//!
//! solomon has no IR: each backend walks the typed AST and emits machine code
//! directly. That makes it easy for the two backends to drift on *decisions* that
//! must agree — most sharply the `printf` runtime ABI (the packed flag bits and the
//! conversion→radix mapping), which both backends bake into their emitted
//! formatters and which therefore has to be byte-for-byte identical for the
//! interpreter-as-oracle conformance to hold. This module is the single source of
//! truth for those pure decisions. Everything here returns plain data and emits
//! nothing, so it cannot change a single output byte on its own — the backends
//! consume the results and do the actual instruction emission.

use std::collections::HashMap;

use crate::ast::{Expr, ExprKind, Stmt, StmtKind, Type};
use crate::codegen::CodegenError;
use crate::fmt::Spec;
use crate::layout::Layouts;
use crate::token::Pos;

/// An aggregate (class/union or array) is represented by its address — it never
/// lives in a register — so it is passed/copied by reference. Shared by both
/// backends and the [`gen_init_into`] driver.
pub fn is_aggregate(ty: &Type) -> bool {
    matches!(ty, Type::Named(_) | Type::Array(..))
}

/// A backend's hooks for the shared brace/designated-initializer lowering
/// ([`gen_init_into`]). The recursion, field/offset collection, and leaf dispatch
/// are identical across targets and live in the driver; a backend supplies only the
/// three *leaf stores* and the bits the driver can't compute itself. `Place` is the
/// backend's own "where the aggregate lives" type (a frame slot or a global). This
/// is a TCC-style emitter vtable — emission stays immediate, there is no IR.
pub trait InitEmitter {
    /// Where the aggregate being initialised lives (a frame slot or global), opaque
    /// to the driver, which only forwards it to the leaf stores.
    type Place: Copy;
    /// The backend's name for diagnostics, e.g. `"arm64 backend"`.
    fn backend_label(&self) -> &'static str;
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
}

/// Emit the stores for a brace/designated initialiser (or a single leaf value) into
/// the aggregate at `place`, at byte offset `byte_off`. Recurses for nested
/// arrays/classes; only the provided elements/fields are written (the caller has
/// zeroed local slots; globals are linker-zeroed), so partial initialisers leave the
/// rest zero. Backend-independent: the per-leaf machine stores are the backend's via
/// [`InitEmitter`]. Shared by both native backends so the two can't drift.
pub fn gen_init_into<E: InitEmitter>(
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
                // Collect (type, offset) before the store loop so the immutable layout
                // borrow ends before the recursive `&mut cg` calls.
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
    // A leaf value: float, an aggregate-valued expression, or a scalar/pointer.
    if matches!(ty, Type::F64) {
        cg.emit_float_init(place, byte_off, init)
    } else if is_aggregate(ty) {
        cg.emit_aggregate_init(place, byte_off, ty, init)
    } else {
        cg.emit_scalar_init(place, byte_off, ty, init)
    }
}

/// The relation tested when matching a `switch` value against a case bound, used by
/// [`SwitchEmitter::switch_cmp_branch`]: branch when `switch_value <cc> bound`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwitchCc {
    /// `==` — a single `case` value matched.
    Eq,
    /// `<` — below a `lo ... hi` range's lower bound (skip the case).
    Lt,
    /// `>` — above a range's upper bound (skip the case).
    Gt,
}

/// A backend's hooks for the shared control-flow lowering — `switch` ([`gen_switch`])
/// and the loops/conditionals ([`gen_if`]/[`gen_while`]/[`gen_do_while`]/[`gen_for`]).
/// The control *structure* of each (label placement, the `break`/`continue` target
/// stacks, the switch partition + dispatch, the loop back-edges) is identical across
/// targets and lives in the drivers; a backend supplies only the leaf emits: label
/// ops, conditional branches off an evaluated expression, and statement recursion.
/// This is a TCC-style emitter vtable — emission stays immediate, there is no IR.
pub trait CodeEmitter {
    /// The backend's handle for the frame slot holding the evaluated switch value
    /// (an offset), opaque to the driver.
    type Slot: Copy;
    /// The backend's name for diagnostics, e.g. `"arm64 backend"`.
    fn backend_label(&self) -> &'static str;
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
    /// Try an O(1) jump table instead of the compare-chain. `Ok(true)` if it emitted
    /// the table (the driver then skips the chain); `Ok(false)` to fall back. The
    /// default is no table (the freestanding/x86 path); arm64 overrides it.
    fn try_switch_table(
        &mut self,
        _stmts: &[Stmt],
        _label_at: &HashMap<usize, usize>,
        _slot: Self::Slot,
        _gap_target: usize,
    ) -> Result<bool, CodegenError> {
        Ok(false)
    }
}

/// Lower a `switch`. Backend-independent control structure: evaluate the value to a
/// slot, run any `start:` prologue, assign a label per `case`/`default`, dispatch
/// (an optional jump table, else a compare-chain to the matching label, falling
/// through to `default`/the `end:` epilogue/the exit), then emit the body placing
/// each label and the epilogue. The leaf emits are the backend's via
/// [`SwitchEmitter`]. Shared by both native backends so they can't drift on the
/// subtle `start:`/`end:` and gap-target semantics.
pub fn gen_switch<E: CodeEmitter>(
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

    // `start:` / `end:` sub-labels partition the body into an optional prologue (runs
    // on entry, before dispatch) and epilogue (reached by fall-through; `break` skips
    // it). Sema has checked the ordering.
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

    // No case matched: fall to default, else the epilogue, else the exit.
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

/// Lower an `if`/`if`-`else`: branch past the `then` arm when the condition is
/// false; with an `else`, jump over it after the `then` arm. Backend-independent
/// (label/branch/recursion via [`CodeEmitter`]).
pub fn gen_if<E: CodeEmitter>(
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
/// `continue` targets the top (the re-test).
pub fn gen_while<E: CodeEmitter>(cg: &mut E, cond: &Expr, body: &Stmt) -> Result<(), CodegenError> {
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

/// Lower a `do`-`while`: run the body, then test at the bottom and loop back while
/// true. `continue` targets the bottom test (`l_cont`).
pub fn gen_do_while<E: CodeEmitter>(
    cg: &mut E,
    body: &Stmt,
    cond: &Expr,
) -> Result<(), CodegenError> {
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

/// A backend's hooks for the shared call lowering ([`gen_call`]). The call *shape* —
/// spill an indirect callee, allocate an sret slot, evaluate and spill each named
/// argument, place the arguments in registers, set the sret pointer, emit the call,
/// deliver the result — is identical across targets and lives in the driver. The
/// register/stack *strategy* of placing the arguments genuinely differs (most
/// sharply, the two backends marshal variadics differently — direct address
/// computation vs a stack round-trip), so that one divergent step, [`place_args`],
/// stays a per-backend hook that keeps each target's exact strategy verbatim. The
/// actual call instruction is a closure (each backend's `CallTarget` enum differs).
/// Emitter vtable, no IR.
///
/// [`place_args`]: CallEmitter::place_args
pub trait CallEmitter {
    /// The backend's handle for the sret result slot (a frame offset).
    type Slot: Copy;
    /// Evaluate the indirect-call target and spill it (deepest on the stack) so it
    /// survives argument evaluation; the call-instruction closure pops it back.
    fn spill_callee(&mut self, callee: &Expr) -> Result<(), CodegenError>;
    /// Allocate an sret result slot when `ret` is returned by value, else `None`.
    fn alloc_sret(&mut self, ret: &Type) -> Option<Self::Slot>;
    /// Evaluate one named argument of type `ty` and spill its 8 bytes to the stack.
    fn eval_arg_spill(&mut self, ty: &Type, arg: &Expr) -> Result<(), CodegenError>;
    /// Place the arguments in their ABI registers: stage the trailing variadic
    /// `extra` args (when `varargs`, even with none — the hidden count is 0), pop the
    /// named args (per `classes`, in reverse) into their registers, and place the
    /// hidden variadic `(ptr, count)` pair. Encapsulates each backend's marshalling.
    fn place_args(
        &mut self,
        classes: &[ArgClass],
        extra: &[&Expr],
        varargs: bool,
        pos: Pos,
    ) -> Result<(), CodegenError>;
    /// Set the sret pointer register from `slot`, just before the call (no-op if
    /// `None` — a non-aggregate return).
    fn set_sret_reg(&mut self, slot: Option<Self::Slot>);
    /// Deliver the call result into the expression-evaluation register(s) (the sret
    /// temp's address for an aggregate return, else the float/integer result reg).
    fn deliver_result(&mut self, ret: &Type, sret: Option<Self::Slot>);
    /// Snapshot the frame allocator's bump pointer, so a call's transient scratch
    /// (the variadic buffer) can be reclaimed after the call. The high-water frame
    /// size is tracked separately, so reclaiming never shrinks the frame.
    fn frame_mark(&self) -> u32;
    /// Reset the bump pointer to a `frame_mark` snapshot, reclaiming the variadic
    /// buffer. Reused by the next sequential call; a *nested* call allocated above
    /// this mark, so the two never overlap.
    fn frame_reset(&mut self, mark: u32);
}

/// Lower a call. Backend-independent shape: spill an indirect callee, allocate the
/// sret slot, evaluate+spill the named args left-to-right, place all args in
/// registers ([`CallEmitter::place_args`], the one target-specific step), set the
/// sret pointer, emit the call instruction (`emit_call_insn`, since the `CallTarget`
/// enums differ), then deliver the result. Shared by both native backends so the
/// call shape can't drift.
#[allow(clippy::too_many_arguments)]
pub fn gen_call<E: CallEmitter>(
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
    // reclaim it: without this, a function with many variadic calls (e.g. hundreds of
    // `Print`s) would grow its frame by one buffer per call and overflow.
    let mark = cg.frame_mark();
    cg.place_args(classes, extra, varargs, pos)?;
    cg.set_sret_reg(sret);
    emit_call_insn(cg);
    cg.frame_reset(mark);
    cg.deliver_result(ret, sret);
    Ok(())
}

/// Lower a `for`: optional init in a fresh scope, top test, body, `continue` target
/// at the step, step, loop back. An absent condition loops unconditionally.
pub fn gen_for<E: CodeEmitter>(
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
/// in the `n`-th integer/pointer register, or the `n`-th floating-point register.
/// The two classes are numbered independently (as in both AAPCS64 and System V).
/// Register *names* and the per-class register *counts* are each backend's concern;
/// this is just the shared classification *sequence*, which must agree so a caller
/// and callee built by different backends would still match.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArgClass {
    /// The `n`-th integer/pointer argument register (also carries by-address
    /// aggregates).
    Int(usize),
    /// The `n`-th floating-point argument register.
    Float(usize),
}

/// Which register class ran out in [`classify_args`], so the caller can raise its
/// own (target-named) diagnostic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArgOverflow {
    Int,
    Float,
}

/// Classify each argument by its parameter type: an `F64` takes the next
/// floating-point register, everything else (integer/pointer, or a by-address
/// aggregate) the next integer register. `max_int`/`max_fp` bound each class
/// (AAPCS64 is 8/8, System V is 6/8); on overflow the offending class is returned.
/// The two counters advance independently, matching both target ABIs.
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

/// The packed flag bits passed to the emitted `printf` runtime (`FmtInt`/`FmtStr`/
/// the float formatters) in both backends. The bit *values* are a shared runtime
/// ABI: [`spec_flags`] sets them and each backend's emitted formatter tests them,
/// so the two definitions must agree. Defined once here as `i64`; the x86-64
/// backend re-exposes them as `i32` derived from these (so they can't drift).
pub const F_SIGNED: i64 = 1; // a signed conversion (`%d`/`%i`): emit a sign, magnitude in digits
pub const F_UPPER: i64 = 2; // uppercase hex (`%X`) and `0X` prefix
pub const F_MINUS: i64 = 4; // left-justify
pub const F_ZERO: i64 = 8; // zero-pad
pub const F_PLUS: i64 = 16; // always show a sign
pub const F_SPACE: i64 = 32; // space before a non-negative
pub const F_HASH: i64 = 64; // alternate form (`0x`/leading `0`)

/// Pack a parsed [`Spec`]'s presentation flags into the runtime flag word (the
/// `F_*` bits above). The conversion-derived flags (`F_SIGNED`/`F_UPPER`) are added
/// by the caller via [`int_conv`], since they depend on the conversion character,
/// not the flag run.
pub fn spec_flags(spec: &Spec) -> i64 {
    let mut flags = 0;
    if spec.minus {
        flags |= F_MINUS;
    }
    if spec.plus {
        flags |= F_PLUS;
    }
    if spec.space {
        flags |= F_SPACE;
    }
    if spec.zero {
        flags |= F_ZERO;
    }
    if spec.hash {
        flags |= F_HASH;
    }
    flags
}

/// The `(radix, extra_flags)` for an integer conversion (`d i u x X o`). The extra
/// flags fold the conversion's signedness/case into the flag word: `%d`/`%i` are
/// `F_SIGNED`, `%X` is `F_UPPER`, the rest add nothing.
pub fn int_conv(conv: char) -> (i64, i64) {
    match conv {
        'd' | 'i' => (10, F_SIGNED),
        'u' => (10, 0),
        'x' => (16, 0),
        'X' => (16, F_UPPER),
        _ => (8, 0), // 'o'
    }
}
