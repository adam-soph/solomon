//! Loop-invariant code motion — a shared IR→IR pass run on the backend path only
//! (`lower_to_machine_ir`, after `lower`, before `destruct_program`), so both native
//! backends benefit while the interpreter keeps consuming the unoptimized IR as the
//! pristine oracle. The 940-case golden suite validates that hoisting never changes output.
//!
//! **Hoist target.** Rather than synthesize a preheader (which would require rewriting the
//! header's phis), an invariant instruction is moved to the end of the loop header's
//! *immediate dominator* — a block that is always outside the loop and dominates it, so the
//! moved def still dominates all its in-loop uses. This needs no new blocks and no phi
//! surgery (SSA stays valid by construction).
//!
//! **Safety.** Only **pure, non-trapping** instructions are hoisted: arithmetic/bit ops
//! (`Bin` except an *integer* `Div`/`Mod`, which could divide by an invariant zero that a
//! zero-trip loop would never reach), `Un`, `Cast`, `Cmp`, `PtrAdd`, and the address ops.
//! Never `Load`/`Store` (memory may change or fault), `Call`/`Prim`/`Mem*` (effects). An
//! instruction is hoisted only when every operand is already defined outside the loop, so a
//! round never reorders interdependent hoists; chains resolve over successive rounds.

use crate::backend::analysis::{self, Cfg, DomTree, LoopForest};
use crate::ir::*;
use std::collections::HashSet;

const NONE: BlockId = u32::MAX;

/// Run LICM over every function in `p`, returning the optimized program.
pub(crate) fn run(mut p: IrProgram) -> IrProgram {
    for f in &mut p.funcs {
        run_func(f);
    }
    p
}

fn run_func(f: &mut IrFunc) {
    let cfg = Cfg::new(f);
    let dom = DomTree::new(f, &cfg);
    let loops = LoopForest::new(f, &cfg, &dom);

    // Snapshot each loop's (hoist target, body) innermost-first, then drop the analyses so we
    // can mutate `f`. Innermost-first lets an outer pass re-hoist what an inner pass exposed.
    let mut order: Vec<usize> = (0..loops.loops.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(loops.depth[loops.loops[i].header as usize]));
    let mut work: Vec<(BlockId, HashSet<BlockId>)> = Vec::new();
    for &li in &order {
        let l = &loops.loops[li];
        let target = dom.idom[l.header as usize];
        // No valid out-of-loop target (header is the function entry): skip this loop.
        if target == NONE || l.body.contains(&target) {
            continue;
        }
        work.push((target, l.body.clone()));
    }
    drop((loops, dom, cfg));

    for (target, body) in work {
        hoist_loop(f, target, &body);
    }
}

/// Hoist invariant instructions out of `body` to the end of `target`, to a fixpoint.
fn hoist_loop(f: &mut IrFunc, target: BlockId, body: &HashSet<BlockId>) {
    let body_blocks: Vec<BlockId> = body.iter().copied().collect();
    loop {
        let def_block = def_blocks(f);
        // A vreg is loop-invariant iff it is not defined inside the loop body.
        let invariant = |v: Vreg| {
            let db = def_block[v as usize];
            db == NONE || !body.contains(&db)
        };

        let mut hoisted: Vec<IrInst> = Vec::new();
        for &b in &body_blocks {
            let blk = &mut f.blocks[b as usize];
            let mut kept = Vec::with_capacity(blk.insts.len());
            for inst in blk.insts.drain(..) {
                if hoistable(&inst) && all_uses_invariant(&inst, invariant) {
                    hoisted.push(inst);
                } else {
                    kept.push(inst);
                }
            }
            blk.insts = kept;
        }
        if hoisted.is_empty() {
            break;
        }
        // Append before the target's terminator (its operands all dominate `target`).
        f.blocks[target as usize].insts.extend(hoisted);
    }
}

/// The block defining each vreg: params at the entry, phis at their block, instructions at
/// theirs. `NONE` for a vreg with no definition (an immediate-only / unused index).
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
            if let Some((d, _)) = analysis::def_of(inst) {
                def[d as usize] = b.id;
            }
        }
    }
    def
}

/// Whether `i` is a pure, non-trapping instruction LICM may relocate.
fn hoistable(i: &IrInst) -> bool {
    match i {
        // Integer divide/modulo can fault on an invariant zero divisor; a zero-trip loop
        // would never reach it, so hoisting could introduce a fault. (Float div/mod yield
        // inf/NaN — no trap — so they are fine.)
        IrInst::Bin { op, ty, .. } => {
            !(matches!(op, IrBinOp::Div | IrBinOp::Mod) && !ty.is_float())
        }
        IrInst::Un { .. }
        | IrInst::Cast { .. }
        | IrInst::Cmp { .. }
        | IrInst::PtrAdd { .. }
        | IrInst::SlotAddr { .. }
        | IrInst::GlobalAddr { .. }
        | IrInst::StrAddr { .. }
        | IrInst::FuncAddr { .. } => true,
        _ => false,
    }
}

/// Whether every vreg `i` reads is loop-invariant (immediates contribute no vreg).
fn all_uses_invariant(i: &IrInst, invariant: impl Fn(Vreg) -> bool) -> bool {
    let mut ok = true;
    analysis::uses_of(i, |r| {
        if !invariant(r) {
            ok = false;
        }
    });
    ok
}
