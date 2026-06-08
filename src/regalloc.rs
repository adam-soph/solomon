//! Out-of-SSA destruction and (eventually) register allocation for the native
//! backends.
//!
//! The native backends emit straight-line machine code, so they need the SSA IR's
//! `phi` nodes resolved into ordinary register copies first. [`destruct_ssa`] does
//! that: for each `phi` it inserts copies on the incoming control-flow edges,
//! splitting a *critical* edge (a branch with several successors into a join with
//! several predecessors) with a fresh block so the copies land only on that edge.
//! The result is a `phi`-free [`IrFunc`] (no longer single-static-assignment) that a
//! backend can lower block by block.
//!
//! Copies on one edge are *parallel* (they all read the predecessor's values, then
//! all write the successor's), so a naive sequential emission could clobber a value
//! still needed (the classic swap/cycle hazard). This implementation sidesteps it by
//! reading every source into a fresh temporary first, then writing every destination
//! from its temporary — always correct, cycles included.
//!
//! [`plan_registers`] is the register *promotion* pass that runs after destruction: a
//! liveness-based linear scan over the `phi`-free form that lifts hot vregs into the
//! target's callee-saved registers (passed in as pools — arm64: x19–x28 / d8–d15; x86-64:
//! rbx/r12–r14 / none, since System V has no callee-saved xmm), returning
//! `vreg → Option<PReg>` (`None` stays in a frame slot). Everything not promoted falls back
//! to the emitter's spill-everything model, so the pass is purely additive — a vreg left
//! unpromoted is emitted exactly as before. A function containing a `try` is left fully
//! spilled (a `throw`'s longjmp would not restore callee-saved registers).

use crate::ir::*;

/// Resolve all `phi` nodes in `f`, returning an equivalent `phi`-free function.
pub fn destruct_ssa(f: &IrFunc) -> IrFunc {
    let mut blocks: Vec<IrBlock> = f.blocks.clone();
    let mut next_vreg = f.n_vregs;

    // Predecessors, from the original terminators (before any edge splitting).
    let mut preds: Vec<Vec<BlockId>> = vec![Vec::new(); blocks.len()];
    for b in &blocks {
        for s in b.term.successors() {
            preds[s as usize].push(b.id);
        }
    }

    let mut split_blocks: Vec<IrBlock> = Vec::new();
    for b_id in 0..blocks.len() {
        let phis = std::mem::take(&mut blocks[b_id].phis);
        if phis.is_empty() {
            continue;
        }
        for a in preds[b_id].clone() {
            // The parallel copies for edge A → B: each phi's destination takes the
            // value the phi names for predecessor A.
            let copies: Vec<(Vreg, IrTy, Val)> = phis
                .iter()
                .map(|phi| {
                    let val = phi
                        .args
                        .iter()
                        .find(|(p, _)| *p == a)
                        .map(|(_, v)| *v)
                        .unwrap_or(Val::ImmInt(0));
                    (phi.dst, phi.ty, val)
                })
                .collect();
            let movs = sequence_copies(&copies, &mut next_vreg);

            if blocks[a as usize].term.successors().len() == 1 {
                // The only edge out of A is A → B: place copies at the end of A.
                blocks[a as usize].insts.extend(movs);
            } else {
                // Critical edge: split it with a new block carrying the copies.
                let s_id = (blocks.len() + split_blocks.len()) as BlockId;
                split_blocks.push(IrBlock {
                    id: s_id,
                    phis: Vec::new(),
                    insts: movs,
                    term: IrTerm::Br(b_id as BlockId),
                });
                redirect_term(&mut blocks[a as usize].term, b_id as BlockId, s_id);
            }
        }
    }
    blocks.extend(split_blocks);

    IrFunc {
        name: f.name.clone(),
        ret: f.ret,
        params: f.params.clone(),
        varargs: f.varargs,
        slots: f.slots.clone(),
        blocks,
        entry: f.entry,
        n_vregs: next_vreg,
    }
}

/// Out-of-SSA every function in a program.
pub fn destruct_program(p: &IrProgram) -> IrProgram {
    IrProgram {
        funcs: p.funcs.iter().map(destruct_ssa).collect(),
        globals: p.globals.clone(),
        strings: p.strings.clone(),
        layouts: p.layouts.clone(),
    }
}

/// Sequence a set of parallel copies (`dst_i = src_i`, all conceptually simultaneous)
/// into `Mov`s: read each source into a fresh temporary, then write each destination
/// from its temporary. Correct for any dependency pattern, swaps and cycles included.
fn sequence_copies(copies: &[(Vreg, IrTy, Val)], next_vreg: &mut u32) -> Vec<IrInst> {
    let mut movs = Vec::with_capacity(copies.len() * 2);
    let mut temps = Vec::with_capacity(copies.len());
    for (_, ty, src) in copies {
        let t = *next_vreg;
        *next_vreg += 1;
        movs.push(IrInst::Mov {
            dst: t,
            ty: *ty,
            src: *src,
        });
        temps.push(t);
    }
    for (i, (dst, ty, _)) in copies.iter().enumerate() {
        movs.push(IrInst::Mov {
            dst: *dst,
            ty: *ty,
            src: Val::Reg(temps[i]),
        });
    }
    movs
}

/// Redirect every `from` successor of a terminator to `to` (for edge splitting).
fn redirect_term(term: &mut IrTerm, from: BlockId, to: BlockId) {
    let swap = |b: &mut BlockId| {
        if *b == from {
            *b = to;
        }
    };
    match term {
        IrTerm::Br(b) => swap(b),
        IrTerm::CondBr { t, f, .. } => {
            swap(t);
            swap(f);
        }
        IrTerm::Switch { cases, default, .. } => {
            for (_, _, b) in cases {
                swap(b);
            }
            swap(default);
        }
        IrTerm::Ret(_) | IrTerm::Throw(_) | IrTerm::Rethrow | IrTerm::Unreachable => {}
    }
}

// ---- register promotion (liveness + linear scan) ----

/// A physical-register assignment for an SSA vreg: a callee-saved GPR (x19–x28) or FP
/// register (d8–d15). `None` (from [`plan_registers`]) means the vreg stays in a frame
/// slot.
#[derive(Clone, Copy)]
pub struct PReg {
    pub is_float: bool,
    pub num: u32,
}

/// The vreg a (non-aggregate) instruction defines, and whether it is a float, derived
/// from the instruction's result type.
fn def_of(i: &IrInst) -> Option<(Vreg, bool)> {
    match i {
        IrInst::Bin { dst, ty, .. } | IrInst::Un { dst, ty, .. } | IrInst::Mov { dst, ty, .. } => {
            Some((*dst, ty.is_float()))
        }
        IrInst::Cast { dst, to, .. } => Some((*dst, to.is_float())),
        IrInst::Load { dst, ty, .. } => Some((*dst, ty.is_float())),
        IrInst::Cmp { dst, .. }
        | IrInst::SlotAddr { dst, .. }
        | IrInst::GlobalAddr { dst, .. }
        | IrInst::StrAddr { dst, .. }
        | IrInst::FuncAddr { dst, .. }
        | IrInst::PtrAdd { dst, .. } => Some((*dst, false)),
        IrInst::Call { dst, ret, .. } => {
            dst.map(|d| (d, matches!(ret, IrRet::Scalar(t) if t.is_float())))
        }
        IrInst::Prim { dst, .. } => dst.map(|d| (d, false)),
        IrInst::Store { .. }
        | IrInst::MemCpy { .. }
        | IrInst::MemZero { .. }
        | IrInst::TryBegin { .. }
        | IrInst::TryEnd => None,
    }
}

/// Visit each vreg an instruction reads.
fn uses_of(i: &IrInst, mut f: impl FnMut(Vreg)) {
    let mut v = |val: &Val| {
        if let Val::Reg(r) = val {
            f(*r)
        }
    };
    match i {
        IrInst::Bin { lhs, rhs, .. } | IrInst::Cmp { lhs, rhs, .. } => {
            v(lhs);
            v(rhs);
        }
        IrInst::Un { src, .. } | IrInst::Cast { src, .. } | IrInst::Mov { src, .. } => v(src),
        IrInst::PtrAdd { base, index, .. } => {
            v(base);
            v(index);
        }
        IrInst::Load { addr, .. } => v(addr),
        IrInst::Store { addr, val, .. } => {
            v(addr);
            v(val);
        }
        IrInst::MemCpy { dst, src, .. } => {
            v(dst);
            v(src);
        }
        IrInst::MemZero { dst, .. } => v(dst),
        IrInst::Call { args, sret, .. } => {
            if let Some(s) = sret {
                v(s);
            }
            for a in args {
                v(&a.val);
            }
        }
        IrInst::Prim { args, .. } => {
            for a in args {
                v(a);
            }
        }
        IrInst::SlotAddr { .. }
        | IrInst::GlobalAddr { .. }
        | IrInst::StrAddr { .. }
        | IrInst::FuncAddr { .. }
        | IrInst::TryBegin { .. }
        | IrInst::TryEnd => {}
    }
}

/// Visit each vreg a terminator reads.
fn term_uses(t: &IrTerm, mut f: impl FnMut(Vreg)) {
    let mut v = |val: &Val| {
        if let Val::Reg(r) = val {
            f(*r)
        }
    };
    match t {
        IrTerm::CondBr { cond, .. } => match cond {
            Cond::NonZero { val, .. } => v(val),
            Cond::Cmp { lhs, rhs, .. } => {
                v(lhs);
                v(rhs);
            }
        },
        IrTerm::Switch { val, .. } => v(val),
        IrTerm::Ret(Some(val)) | IrTerm::Throw(val) => v(val),
        IrTerm::Br(_) | IrTerm::Ret(None) | IrTerm::Rethrow | IrTerm::Unreachable => {}
    }
}

/// Plan register promotion for `f` (already out of SSA): a liveness-based linear scan that
/// assigns hot vregs to the target's callee-saved registers, returning `vreg → Option<PReg>`
/// (`None` = stays spilled). `int_regs`/`float_regs` are the allocatable callee-saved GPR /
/// FP register numbers (arm64: x19–x28 / d8–d15; x86-64: rbx/r12–r15 / none, since System V
/// has no callee-saved xmm). An empty pool means that class is never promoted. A function
/// containing a `try` gets **no** promotion: a `throw`'s longjmp restores sp/fp but not
/// callee-saved registers, so a promoted local could be clobbered by an abandoned callee —
/// spill-all is always correct.
pub fn plan_registers(f: &IrFunc, int_regs: &[u32], float_regs: &[u32]) -> Vec<Option<PReg>> {
    let n = f.n_vregs as usize;
    let mut plan: Vec<Option<PReg>> = vec![None; n];
    let has_try = f.blocks.iter().any(|b| {
        matches!(b.term, IrTerm::Throw(_) | IrTerm::Rethrow)
            || b.insts
                .iter()
                .any(|i| matches!(i, IrInst::TryBegin { .. } | IrInst::TryEnd))
    });
    if n == 0 || has_try {
        return plan;
    }

    // Float/int class per vreg (a wrong class would put a float in an x-register).
    let mut is_float = vec![false; n];
    for p in &f.params {
        if matches!(p.ty, ArgTy::Float) {
            is_float[p.vreg as usize] = true;
        }
    }
    for b in &f.blocks {
        for i in &b.insts {
            if let Some((d, fl)) = def_of(i) {
                is_float[d as usize] = fl;
            }
        }
    }

    let nb = f.blocks.len();
    // Linear instruction positions: each block occupies `insts.len()` slots then one for
    // its terminator.
    let mut blk_start = vec![0u32; nb];
    let mut blk_term = vec![0u32; nb];
    let mut pos = 0u32;
    for (bi, b) in f.blocks.iter().enumerate() {
        blk_start[bi] = pos;
        pos += b.insts.len() as u32;
        blk_term[bi] = pos;
        pos += 1;
    }

    // Per-block use/def for the liveness dataflow.
    let mut use_set: Vec<std::collections::HashSet<Vreg>> = vec![Default::default(); nb];
    let mut def_set: Vec<std::collections::HashSet<Vreg>> = vec![Default::default(); nb];
    for (bi, b) in f.blocks.iter().enumerate() {
        let mut defined: std::collections::HashSet<Vreg> = Default::default();
        for i in &b.insts {
            uses_of(i, |r| {
                if !defined.contains(&r) {
                    use_set[bi].insert(r);
                }
            });
            if let Some((d, _)) = def_of(i) {
                defined.insert(d);
                def_set[bi].insert(d);
            }
        }
        term_uses(&b.term, |r| {
            if !defined.contains(&r) {
                use_set[bi].insert(r);
            }
        });
    }

    // Backward liveness to a fixpoint.
    let succs: Vec<Vec<usize>> = f
        .blocks
        .iter()
        .map(|b| b.term.successors().iter().map(|&s| s as usize).collect())
        .collect();
    let mut live_in: Vec<std::collections::HashSet<Vreg>> = vec![Default::default(); nb];
    let mut live_out: Vec<std::collections::HashSet<Vreg>> = vec![Default::default(); nb];
    loop {
        let mut changed = false;
        for bi in (0..nb).rev() {
            let mut out: std::collections::HashSet<Vreg> = Default::default();
            for &s in &succs[bi] {
                out.extend(live_in[s].iter().copied());
            }
            let mut in_ = use_set[bi].clone();
            for &r in &out {
                if !def_set[bi].contains(&r) {
                    in_.insert(r);
                }
            }
            if out != live_out[bi] || in_ != live_in[bi] {
                live_out[bi] = out;
                live_in[bi] = in_;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    // Single live interval [start, end] + a reference count per vreg.
    let mut start = vec![u32::MAX; n];
    let mut end = vec![0u32; n];
    let mut refs = vec![0u32; n];
    for p in &f.params {
        start[p.vreg as usize] = 0;
    }
    for (bi, b) in f.blocks.iter().enumerate() {
        for &r in &live_in[bi] {
            let v = r as usize;
            start[v] = start[v].min(blk_start[bi]);
            end[v] = end[v].max(blk_start[bi]);
        }
        for &r in &live_out[bi] {
            let v = r as usize;
            start[v] = start[v].min(blk_term[bi]);
            end[v] = end[v].max(blk_term[bi]);
        }
        let mut p = blk_start[bi];
        for i in &b.insts {
            uses_of(i, |r| {
                let v = r as usize;
                start[v] = start[v].min(p);
                end[v] = end[v].max(p);
                refs[v] += 1;
            });
            if let Some((d, _)) = def_of(i) {
                let v = d as usize;
                start[v] = start[v].min(p);
                end[v] = end[v].max(p);
            }
            p += 1;
        }
        term_uses(&b.term, |r| {
            let v = r as usize;
            start[v] = start[v].min(p);
            end[v] = end[v].max(p);
            refs[v] += 1;
        });
    }

    // Promotion candidates: referenced at least twice, with a real interval.
    let mut cands: Vec<usize> = (0..n)
        .filter(|&v| start[v] != u32::MAX && refs[v] >= 2)
        .collect();
    cands.sort_by_key(|&v| (start[v], std::cmp::Reverse(refs[v])));

    // Free pools (held so `pop` hands out the first-listed register first).
    let mut int_free: Vec<u32> = int_regs.iter().rev().copied().collect();
    let mut flt_free: Vec<u32> = float_regs.iter().rev().copied().collect();
    // active: (end, reg, is_float, refs, vreg)
    let mut active: Vec<(u32, u32, bool, u32, usize)> = Vec::new();

    for &v in &cands {
        let (s, e, fl, rf) = (start[v], end[v], is_float[v], refs[v]);
        // Expire intervals that ended before this one starts.
        let mut i = 0;
        while i < active.len() {
            if active[i].0 < s {
                let (_, reg, afl, _, _) = active.remove(i);
                if afl {
                    flt_free.push(reg);
                } else {
                    int_free.push(reg);
                }
            } else {
                i += 1;
            }
        }
        let pool = if fl { &mut flt_free } else { &mut int_free };
        if let Some(reg) = pool.pop() {
            plan[v] = Some(PReg {
                is_float: fl,
                num: reg,
            });
            active.push((e, reg, fl, rf, v));
        } else {
            // Pool full: evict the coldest active of this class if it is strictly colder.
            let victim = active
                .iter()
                .enumerate()
                .filter(|(_, a)| a.2 == fl && a.3 < rf)
                .min_by_key(|(_, a)| a.3)
                .map(|(idx, _)| idx);
            if let Some(idx) = victim {
                let (_, reg, _, _, loser) = active.remove(idx);
                plan[loser] = None;
                plan[v] = Some(PReg {
                    is_float: fl,
                    num: reg,
                });
                active.push((e, reg, fl, rf, v));
            }
        }
    }
    plan
}

#[cfg(test)]
mod tests {
    use crate::irinterp::IrInterp;

    /// Parse → sema → layout → lower a program to SSA IR.
    fn lower(src: &str) -> crate::ir::IrProgram {
        let prog = crate::parser::parse(src).expect("parse");
        assert!(
            crate::sema::check_program(&prog).is_empty(),
            "sema errors in test source"
        );
        let (layouts, errs) = crate::layout::compute(&prog);
        assert!(errs.is_empty(), "layout errors");
        crate::lower::lower(&prog, &layouts).expect("lower")
    }

    fn run(p: &crate::ir::IrProgram) -> String {
        IrInterp::new(p).run_program().expect("run")
    }

    /// Out-of-SSA must preserve a program's observable output, and remove every phi.
    #[test]
    fn out_of_ssa_preserves_output() {
        // Programs chosen to create phis: loops, ternaries, short-circuits, switch
        // fall-through, and a loop that swaps two variables (a parallel-copy cycle).
        let sources = [
            "I64 i, s = 0; for (i = 1; i <= 10; i++) s += i; \"%d\\n\", s;",
            "I64 n = 7, a = 0, b = 1, i; for (i = 0; i < n; i++) { I64 t = a + b; a = b; b = t; } \"%d %d\\n\", a, b;",
            "I64 x = 5; \"%d\\n\", (x > 0 && x < 10) ? x * 2 : -1;",
            "I64 i; for (i = 0; i < 6; i++) switch (i) { case 1 ... 3: \"m%d\\n\", i; default: \"d%d\\n\", i; }",
            // A swap inside a loop body exercises cyclic parallel copies at the header.
            "I64 a = 1, b = 2, i; for (i = 0; i < 3; i++) { I64 t = a; a = b; b = t; } \"%d %d\\n\", a, b;",
        ];
        for src in sources {
            let ssa = lower(src);
            let before = run(&ssa);
            let flat = super::destruct_program(&ssa);
            for f in &flat.funcs {
                assert!(
                    f.blocks.iter().all(|b| b.phis.is_empty()),
                    "fn {} still has phis after destruction",
                    f.name
                );
            }
            let after = run(&flat);
            assert_eq!(before, after, "out-of-SSA changed output for:\n{src}");
        }
    }

    /// A hot loop must actually promote some vreg to a callee-saved register — guarding
    /// against a regression where `plan_registers` silently falls back to spill-all
    /// (which would still pass conformance but lose the optimization). The accumulator
    /// and induction variable are live across the loop and referenced repeatedly.
    #[test]
    fn promotes_hot_loop_vregs() {
        let ssa = super::destruct_program(&lower(
            "I64 i, s = 0; for (i = 0; i < 100; i++) s += i * i;",
        ));
        let main = ssa
            .funcs
            .iter()
            .find(|f| f.name == crate::lower::ENTRY)
            .expect("entry");
        let plan = super::plan_registers(
            main,
            &[19, 20, 21, 22, 23, 24, 25, 26, 27, 28],
            &[8, 9, 10, 11, 12, 13, 14, 15],
        );
        let promoted = plan.iter().filter(|p| p.is_some()).count();
        assert!(promoted > 0, "expected some vreg promoted to a register");
        // Every promotion lands in the callee-saved pools (x19–x28 / d8–d15).
        for p in plan.iter().flatten() {
            if p.is_float {
                assert!(
                    (8..=15).contains(&p.num),
                    "float reg out of d8–d15: {}",
                    p.num
                );
            } else {
                assert!(
                    (19..=28).contains(&p.num),
                    "int reg out of x19–x28: {}",
                    p.num
                );
            }
        }
    }

    /// A function containing a `try` gets no promotion (a `throw`'s longjmp would not
    /// restore callee-saved registers, clobbering a promoted local).
    #[test]
    fn no_promotion_in_try_functions() {
        let ssa = super::destruct_program(&lower(
            "I64 i, s = 0; for (i = 0; i < 50; i++) s += i; try { throw 1; } catch { s++; } \"%d\\n\", s;",
        ));
        let main = ssa
            .funcs
            .iter()
            .find(|f| f.name == crate::lower::ENTRY)
            .expect("entry");
        let plan = super::plan_registers(
            main,
            &[19, 20, 21, 22, 23, 24, 25, 26, 27, 28],
            &[8, 9, 10, 11, 12, 13, 14, 15],
        );
        assert!(
            plan.iter().all(|p| p.is_none()),
            "a try-containing function must not promote any vreg"
        );
    }
}
