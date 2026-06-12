//! Dead-code elimination on the shared SSA IR (backend path only — the interpreter keeps the
//! unoptimized IR as the oracle). A mark-sweep over pure values: an instruction with side
//! effects is a root and is always kept; a pure instruction (or `phi`) is kept only if its
//! result is transitively reached from a root or a terminator. The mark-sweep (rather than a
//! use-count fixpoint) collects **dead `phi` cycles** — two mutually-referencing phis that
//! nothing else uses are never reached from a root, so both are swept.
//!
//! `Load` is treated as a root (kept even if its result is dead): a load through a computed
//! pointer could fault, and DCE has no aliasing proof that the address is mapped. CSE removes
//! the loads it *can* prove redundant; DCE only removes pure register computation.

use crate::backend::analysis::{def_of, term_uses, uses_of};
use crate::ir::*;

pub fn run(mut ir: Program) -> Program {
    for f in &mut ir.funcs {
        run_func(f);
    }
    ir
}

/// Where a vreg is defined, so marking a value live can mark its defining site live.
#[derive(Clone, Copy)]
enum Def {
    Inst(u32, u32),
    Phi(u32, u32),
}

/// A pure instruction may be removed if its result is unused; an effectful one is a root.
fn is_pure(inst: &Inst) -> bool {
    matches!(
        inst,
        Inst::Bin { .. }
            | Inst::Un { .. }
            | Inst::Cmp { .. }
            | Inst::Cast { .. }
            | Inst::Mov { .. }
            | Inst::PtrAdd { .. }
            | Inst::SlotAddr { .. }
            | Inst::GlobalAddr { .. }
            | Inst::StrAddr { .. }
            | Inst::FuncAddr { .. }
    )
}

fn run_func(f: &mut Func) {
    let n = f.n_vregs as usize;
    if n == 0 {
        return;
    }
    let mut def_site: Vec<Option<Def>> = vec![None; n];
    for (bi, b) in f.blocks.iter().enumerate() {
        for (pi, phi) in b.phis.iter().enumerate() {
            def_site[phi.dst as usize] = Some(Def::Phi(bi as u32, pi as u32));
        }
        for (ii, inst) in b.insts.iter().enumerate() {
            if let Some((d, _)) = def_of(inst) {
                def_site[d as usize] = Some(Def::Inst(bi as u32, ii as u32));
            }
        }
    }

    // Mark roots live: every effectful (non-pure) instruction, plus every terminator operand.
    let mut live = vec![false; n];
    let mut work: Vec<Vreg> = Vec::new();
    let mark = |v: Vreg, live: &mut [bool], work: &mut Vec<Vreg>| {
        if !live[v as usize] {
            live[v as usize] = true;
            work.push(v);
        }
    };
    for b in &f.blocks {
        for inst in &b.insts {
            if !is_pure(inst) {
                uses_of(inst, |r| mark(r, &mut live, &mut work));
            }
        }
        term_uses(&b.term, |r| mark(r, &mut live, &mut work));
    }

    // Propagate liveness backward through pure defs and phis.
    while let Some(v) = work.pop() {
        match def_site[v as usize] {
            Some(Def::Inst(bi, ii)) => {
                uses_of(&f.blocks[bi as usize].insts[ii as usize], |r| {
                    mark(r, &mut live, &mut work)
                });
            }
            Some(Def::Phi(bi, pi)) => {
                let args = f.blocks[bi as usize].phis[pi as usize].args.clone();
                for (_, val) in args {
                    if let Some(r) = val.reg() {
                        mark(r, &mut live, &mut work);
                    }
                }
            }
            None => {}
        }
    }

    // Sweep: drop pure instructions and phis whose result is dead.
    for b in &mut f.blocks {
        b.phis.retain(|phi| live[phi.dst as usize]);
        b.insts.retain(|inst| match def_of(inst) {
            Some((d, _)) if is_pure(inst) => live[d as usize],
            _ => true,
        });
    }
}

#[cfg(test)]
#[path = "tests/dce.rs"]
mod tests;
