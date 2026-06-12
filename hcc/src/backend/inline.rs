//! Function inlining on the shared SSA IR (backend path only — the interpreter keeps the
//! unoptimized IR as the oracle). A small, non-recursive callee called directly is spliced
//! into its caller: the call site's block is split, the callee's blocks are cloned in (with
//! vregs / slots / block ids offset into the caller's space), each callee `Ret` becomes a
//! branch to the continuation, and the call's result becomes a `phi` over the returned values.
//! Eliminating the call exposes the callee's body to the caller's CSE / LICM / register
//! promotion (e.g. a hot helper's loop-carried scalars promote in the caller).
//!
//! Strictly scoped for safety: direct calls only; a defined, non-`varargs`, non-recursive
//! callee with a scalar/void return (no sret aggregate) and no exception regions; bounded
//! size; and a per-caller budget so inlining always terminates. `verify()` (debug-asserted in
//! the pipeline) re-checks each result.

use crate::backend::analysis::{Cfg, DomTree};
use crate::ir::*;

/// Largest callee (instruction count) to inline, and the most inlines applied to one caller.
const MAX_CALLEE_INSTS: usize = 40;
const MAX_INLINES_PER_FUNC: usize = 16;

pub fn run(mut ir: Program) -> Program {
    // Clone the callee bodies up front so a caller can be mutated while a callee is read (they
    // may be the same `Vec` element class). Only inlinable-shaped callees are kept.
    let callees: Vec<Func> = ir
        .funcs
        .iter()
        .filter(|f| inlinable_callee(f))
        .cloned()
        .collect();
    if callees.is_empty() {
        return ir;
    }
    for caller in &mut ir.funcs {
        let mut applied = 0;
        while applied < MAX_INLINES_PER_FUNC {
            let Some(site) = find_call_site(caller, &callees) else {
                break;
            };
            let callee = callees
                .iter()
                .find(|c| c.name == site.callee)
                .unwrap()
                .clone();
            inline_one(caller, &callee, site.block, site.inst);
            applied += 1;
        }
    }
    ir
}

/// A callee is inlinable if it is defined (has blocks), is not `varargs`, returns void or a
/// scalar (no sret aggregate), has no exception regions, an entry block with no `phi`s, contains
/// **no loop**, and is not too large.
///
/// The loop exclusion is a deliberate register-pressure guard: inlining a function that has its
/// own hot loop into a caller that is itself inside a loop (the common case — a helper called
/// from a loop body) lengthens the live ranges of *both* loops' carried values, spilling
/// variables that each kept in registers when separate. A loop-free helper has no such carried
/// state, so splicing it in is a clean win; a loop-bearing one stays a callee and keeps its own
/// tight allocation.
fn inlinable_callee(f: &Func) -> bool {
    if f.blocks.is_empty() || f.varargs || matches!(f.ret, Ret::Agg { .. }) {
        return false;
    }
    if f.blocks[f.entry as usize].phis.iter().next().is_some() {
        return false;
    }
    let mut insts = 0;
    for b in &f.blocks {
        for i in &b.insts {
            insts += 1;
            if matches!(i, Inst::TryBegin { .. } | Inst::TryEnd) {
                return false;
            }
        }
    }
    if insts > MAX_CALLEE_INSTS || has_loop(f) {
        return false;
    }
    true
}

/// Whether `f`'s CFG contains a back-edge (a successor that dominates its source) — i.e. a loop.
fn has_loop(f: &Func) -> bool {
    let cfg = Cfg::new(f);
    let dom = DomTree::new(f, &cfg);
    for (u, succs) in cfg.succs.iter().enumerate() {
        for &v in succs {
            if dom.dominates(v, u as BlockId) {
                return true;
            }
        }
    }
    false
}

struct Site {
    block: usize,
    inst: usize,
    callee: String,
}

/// The first inlinable direct call in `caller`: a `Call` to a known inlinable callee (other
/// than the caller itself — no direct recursion), with no sret and matching variadic-free
/// shape.
fn find_call_site(caller: &Func, callees: &[Func]) -> Option<Site> {
    for (bi, b) in caller.blocks.iter().enumerate() {
        for (ii, inst) in b.insts.iter().enumerate() {
            if let Inst::Call {
                callee: Callee::Direct(name),
                sret: None,
                varargs,
                ret,
                ..
            } = inst
            {
                if varargs.is_varargs || name == &caller.name || matches!(ret, Ret::Agg { .. }) {
                    continue;
                }
                if callees.iter().any(|c| &c.name == name) {
                    return Some(Site {
                        block: bi,
                        inst: ii,
                        callee: name.clone(),
                    });
                }
            }
        }
    }
    None
}

/// Add `voff` to every vreg (def + operands) and `soff` to every slot id in `inst`.
fn shift_inst(inst: &mut Inst, voff: u32, soff: u32) {
    if let Some(d) = inst.def_mut() {
        *d += voff;
    }
    inst.for_each_use_mut(|v| {
        if let Val::Reg(r) = v {
            *r += voff;
        }
    });
    if let Inst::SlotAddr { slot, .. } = inst {
        *slot += soff;
    }
}

/// Add `boff` to every block id a terminator targets.
fn shift_term_blocks(term: &mut Term, boff: BlockId) {
    match term {
        Term::Br(b) => *b += boff,
        Term::CondBr { t, f, .. } => {
            *t += boff;
            *f += boff;
        }
        Term::Switch { cases, default, .. } => {
            for (_, _, b) in cases {
                *b += boff;
            }
            *default += boff;
        }
        Term::Ret(_) | Term::Throw(_) | Term::Rethrow | Term::Unreachable => {}
    }
}

/// Splice `callee` into `caller` at `caller.blocks[bi].insts[ci]` (the call). See the module
/// doc; the structure is:
///   bi (call block) → keep insts `0..ci`, bind params, branch to the callee entry clone.
///   callee clones    → appended with offset ids; each `Ret v` becomes `Br(cont)`.
///   cont (new block) → the call's `phi` over returned values + the call block's tail + term.
fn inline_one(caller: &mut Func, callee: &Func, bi: usize, ci: usize) {
    let voff = caller.n_vregs;
    let soff = caller.slots.len() as u32;
    let cont_id = caller.blocks.len() as BlockId;
    let clone_off = cont_id + 1; // callee block ids are shifted past `cont`
    let entry_clone = callee.entry + clone_off;

    // Pull the call apart.
    let Inst::Call { dst, args, .. } = caller.blocks[bi].insts[ci].clone() else {
        unreachable!("inline_one called on a non-call");
    };

    // Split the call block: tail (insts after the call) + original terminator move to `cont`.
    let tail: Vec<Inst> = caller.blocks[bi].insts.split_off(ci + 1);
    caller.blocks[bi].insts.pop(); // drop the call itself
    let orig_term = std::mem::replace(&mut caller.blocks[bi].term, Term::Br(entry_clone));

    // Bind params: `param_clone = arg` at the end of the call block, before the branch.
    for (p, a) in callee.params.iter().zip(args.iter()) {
        caller.blocks[bi].insts.push(Inst::Mov {
            dst: p.vreg + voff,
            ty: arg_ir_ty(&p.ty),
            src: a.val,
        });
    }

    // Clone the callee blocks, offsetting ids/vregs/slots, turning each `Ret` into a branch to
    // `cont` and collecting the returned values for the result phi.
    let mut ret_args: Vec<(BlockId, Val)> = Vec::new();
    let mut cloned: Vec<Block> = Vec::with_capacity(callee.blocks.len());
    for cb in &callee.blocks {
        let new_id = cb.id + clone_off;
        let mut phis: Vec<Phi> = cb.phis.clone();
        for phi in &mut phis {
            phi.dst += voff;
            for (b, v) in &mut phi.args {
                *b += clone_off;
                if let Val::Reg(r) = v {
                    *r += voff;
                }
            }
        }
        let mut insts: Vec<Inst> = cb.insts.clone();
        for inst in &mut insts {
            shift_inst(inst, voff, soff);
        }
        let term = match &cb.term {
            Term::Ret(v) => {
                if let Some(v) = v {
                    let rv = match v {
                        Val::Reg(r) => Val::Reg(*r + voff),
                        other => *other,
                    };
                    ret_args.push((new_id, rv));
                }
                Term::Br(cont_id)
            }
            other => {
                let mut t = other.clone();
                // Offset both the vreg operands (the branch condition / switch value / throw
                // value) and the target block ids.
                t.for_each_use_mut(|val| {
                    if let Val::Reg(r) = val {
                        *r += voff;
                    }
                });
                shift_term_blocks(&mut t, clone_off);
                t
            }
        };
        cloned.push(Block {
            id: new_id,
            phis,
            insts,
            term,
        });
    }

    // Build the continuation block: result phi (if the call had a dst), then the tail + term.
    let mut cont = Block {
        id: cont_id,
        phis: Vec::new(),
        insts: tail,
        term: orig_term,
    };
    if let Some(d) = dst {
        let ty = match callee.ret {
            Ret::Scalar(t) => t,
            _ => Ty::I64,
        };
        cont.phis.push(Phi {
            dst: d,
            ty,
            args: ret_args,
        });
    }

    // Any block the call block originally branched to now has `cont` as its predecessor, not
    // `bi` — fix up their phi predecessor ids.
    for succ in cont.term.successors() {
        for phi in &mut caller.blocks[succ as usize].phis {
            for (b, _) in &mut phi.args {
                if *b == bi as BlockId {
                    *b = cont_id;
                }
            }
        }
    }

    caller.blocks.push(cont);
    caller.blocks.extend(cloned);
    caller.slots.extend(callee.slots.iter().cloned());
    caller.n_vregs += callee.n_vregs;
}

/// The `Ty` to carry a parameter binding `Mov` (the callee reads the param at this width).
fn arg_ir_ty(ty: &ArgTy) -> Ty {
    match ty {
        ArgTy::Int(t) => *t,
        ArgTy::Float => Ty::F64,
        ArgTy::AggAddr { .. } => Ty::Ptr,
    }
}

#[cfg(test)]
#[path = "tests/inline.rs"]
mod tests;
