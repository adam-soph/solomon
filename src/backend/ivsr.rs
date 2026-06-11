//! Induction-variable strength reduction (T6) — a shared IR→IR pass on the backend path only,
//! run after LICM and before out-of-SSA (so the interpreter keeps consuming the unoptimized IR
//! as the oracle).
//!
//! A basic induction variable `k = phi(k0, k + c)` (a header phi whose latch value is the
//! variable plus a constant step `c`) that feeds a `PtrAdd(base, k, stride)` with a
//! loop-invariant `base` and constant `stride` recomputes `base + k*stride` every iteration — a
//! multiply by `stride`. This replaces it with a *parallel* induction variable
//! `q = phi(base + k0*stride, q + c*stride)`, so the multiply becomes a single add on the
//! back-edge and `q` is exactly the row pointer (e.g. matmul's `&b[k][0]` advancing by a row
//! each step). The transform is semantics-preserving: `(k0 + n*c)*stride = k0*stride +
//! n*(c*stride)` under the IR's wrapping two's-complement arithmetic, so `q == base + k*stride`
//! at every iteration; `native == interp` validates it.
//!
//! Conservative shape only: a natural loop with one latch whose header's predecessors are
//! exactly the preheader (the header's immediate dominator, where `q0` is computed) and that
//! latch (where `q`'s step is computed). Anything else is skipped.

use crate::backend::analysis::{Cfg, DomTree, LoopForest};
use crate::ir::*;
use std::collections::HashSet;

const NONE: BlockId = u32::MAX;

/// Run IVSR over every function in `p`.
pub(crate) fn run(mut p: IrProgram) -> IrProgram {
    for f in &mut p.funcs {
        run_func(f);
    }
    p
}

/// One strength-reduction site: replace `PtrAdd { dst: orig, base, index: iv, stride }` with a
/// derived induction variable `q = phi((preheader, base + k0*stride), (latch, q + c*stride))`.
struct Plan {
    orig: Vreg,
    base: Val,
    k0: Val,
    c: i64,
    stride: u32,
    header: BlockId,
    preheader: BlockId,
    latch: BlockId,
}

fn run_func(f: &mut IrFunc) {
    let cfg = Cfg::new(f);
    let dom = DomTree::new(f, &cfg);
    let loops = LoopForest::new(f, &cfg, &dom);
    let def_block = def_blocks(f);

    let mut plans: Vec<Plan> = Vec::new();
    for l in &loops.loops {
        if l.latches.len() != 1 {
            continue;
        }
        let header = l.header;
        let latch = l.latches[0];
        let preheader = dom.idom[header as usize];
        if preheader == NONE || l.body.contains(&preheader) {
            continue;
        }
        // The header's predecessors must be exactly {preheader, latch}, so a derived phi with
        // those two args has the right arity and `q0` (placed in the preheader) is on the only
        // entry edge.
        let preds = &cfg.preds[header as usize];
        if preds.len() != 2 || !preds.contains(&preheader) || !preds.contains(&latch) {
            continue;
        }
        for phi in &f.blocks[header as usize].phis {
            let (Some(k0), Some(Val::Reg(knext))) = (phi_arg(phi, preheader), phi_arg(phi, latch))
            else {
                continue;
            };
            let Some(c) = iv_step(f, &l.body, knext, phi.dst) else {
                continue;
            };
            for &b in &l.body {
                for inst in &f.blocks[b as usize].insts {
                    if let IrInst::PtrAdd {
                        dst,
                        base,
                        index: Val::Reg(idx),
                        stride,
                    } = inst
                    {
                        if *idx == phi.dst && invariant(*base, &def_block, &l.body) {
                            plans.push(Plan {
                                orig: *dst,
                                base: *base,
                                k0,
                                c,
                                stride: *stride,
                                header,
                                preheader,
                                latch,
                            });
                        }
                    }
                }
            }
        }
    }
    drop((cfg, dom, loops));

    // Skip a plan whose `base` is itself another plan's reduced `PtrAdd` (a chained 2D access
    // `a[i][j]` where both indices are induction variables): the inner one still reduces, and
    // the outer keeps using the inner's derived IV. This keeps every applied plan's base a
    // *stable* loop-invariant value that dominates the preheader, so `base + k0*stride` there is
    // well-formed and no plan's base is invalidated by a prior plan's rewrite.
    let origs: HashSet<Vreg> = plans.iter().map(|p| p.orig).collect();
    plans.retain(|p| !matches!(p.base, Val::Reg(r) if origs.contains(&r)));

    for plan in plans {
        apply(f, plan);
    }
}

fn apply(f: &mut IrFunc, p: Plan) {
    let q0 = fresh(f); // base + k0*stride, in the preheader
    let qnext = fresh(f); // q + c*stride, in the latch
    let q = fresh(f); // the derived induction variable (header phi)

    f.blocks[p.preheader as usize].insts.push(IrInst::PtrAdd {
        dst: q0,
        base: p.base,
        index: p.k0,
        stride: p.stride,
    });
    f.blocks[p.latch as usize].insts.push(IrInst::PtrAdd {
        dst: qnext,
        base: Val::Reg(q),
        index: Val::ImmInt(p.c),
        stride: p.stride,
    });
    f.blocks[p.header as usize].phis.push(Phi {
        dst: q,
        ty: IrTy::Ptr,
        args: vec![(p.preheader, Val::Reg(q0)), (p.latch, Val::Reg(qnext))],
    });

    // Drop the original `PtrAdd` and route its uses through the derived IV.
    for b in &mut f.blocks {
        b.insts.retain(|i| i.def() != Some(p.orig));
    }
    replace_all_uses(f, p.orig, Val::Reg(q));
}

/// The phi's argument value flowing in from predecessor `pred`, if any.
fn phi_arg(phi: &Phi, pred: BlockId) -> Option<Val> {
    phi.args.iter().find(|(p, _)| *p == pred).map(|(_, v)| *v)
}

/// If `knext` (a basic IV's latch value) is defined in the loop as `k + c` (or `c + k`) for a
/// constant integer `c`, return `c`.
fn iv_step(f: &IrFunc, body: &HashSet<BlockId>, knext: Vreg, k: Vreg) -> Option<i64> {
    for &b in body {
        for inst in &f.blocks[b as usize].insts {
            if let IrInst::Bin {
                dst,
                op: IrBinOp::Add,
                ty,
                lhs,
                rhs,
                ..
            } = inst
            {
                if *dst == knext && !ty.is_float() {
                    if *lhs == Val::Reg(k) {
                        if let Val::ImmInt(c) = rhs {
                            return Some(*c);
                        }
                    }
                    if *rhs == Val::Reg(k) {
                        if let Val::ImmInt(c) = lhs {
                            return Some(*c);
                        }
                    }
                }
            }
        }
    }
    None
}

/// Whether `v` is loop-invariant: an immediate, or a register defined outside the loop body.
fn invariant(v: Val, def_block: &[BlockId], body: &HashSet<BlockId>) -> bool {
    match v {
        Val::Reg(r) => {
            let db = def_block[r as usize];
            db == NONE || !body.contains(&db)
        }
        _ => true,
    }
}

fn fresh(f: &mut IrFunc) -> Vreg {
    let v = f.n_vregs;
    f.n_vregs += 1;
    v
}

/// The block defining each vreg (params at entry, phis/insts at theirs); `NONE` if undefined.
fn def_blocks(f: &IrFunc) -> Vec<BlockId> {
    let mut def = vec![NONE; f.n_vregs as usize];
    for p in &f.params {
        def[p.vreg as usize] = f.entry;
    }
    for b in &f.blocks {
        for phi in &b.phis {
            def[phi.dst as usize] = b.id;
        }
        for inst in &b.insts {
            if let Some(d) = inst.def() {
                def[d as usize] = b.id;
            }
        }
    }
    def
}

/// Replace every operand use of `Reg(from)` with `to`, across phis, instructions, terminators.
fn replace_all_uses(f: &mut IrFunc, from: Vreg, to: Val) {
    let target = Val::Reg(from);
    for b in &mut f.blocks {
        for phi in &mut b.phis {
            for (_, arg) in &mut phi.args {
                if *arg == target {
                    *arg = to;
                }
            }
        }
        for inst in &mut b.insts {
            inst.for_each_use_mut(|v| {
                if *v == target {
                    *v = to;
                }
            });
        }
        b.term.for_each_use_mut(|v| {
            if *v == target {
                *v = to;
            }
        });
    }
}
