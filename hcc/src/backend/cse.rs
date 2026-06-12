//! Common-subexpression elimination on the shared SSA IR (backend path only — the
//! interpreter keeps the unoptimized IR as the conformance oracle). Three rewrites in one
//! dominator-tree walk:
//!
//! * **No-op cast elimination / copy propagation.** A `Cast` between two 8-byte integer-class
//!   types (`I64`/`U64`/`Ptr`) is a machine no-op on both backends (`emit_int_cast` /
//!   `cast_rax` do nothing for an 8-byte destination), and a `Mov` is a plain copy. Both are
//!   replaced by their source everywhere — folding away the `U64↔I64` reinterpret chains that
//!   default-`I64` arithmetic over `U64` state sprinkles through a hot loop.
//! * **Value numbering** of pure instructions (`Bin`/`Un`/`Cmp`/`Cast`/`PtrAdd` and the address
//!   producers): a redundant computation whose identical predecessor *dominates* it is dropped
//!   and its uses redirected to that predecessor. Commutative operands are canonicalized so
//!   `a+b` and `b+a` collapse.
//! * **Block-local redundant-load elimination.** A `Load` from an address already loaded
//!   (same address value + width) earlier in the same block, with no intervening memory write
//!   or call, reuses the prior result. Any `Store`/`Mem*`/`Call`/`Prim`/`Try*` conservatively
//!   invalidates the whole table.
//!
//! Soundness rests on SSA + dominance: a value substituted in is defined by an instruction
//! that dominates every use being rewritten, so it is in scope and holds the same value.
//! `verify()` (debug-asserted in the pipeline) re-checks the result.

use std::collections::HashMap;

use crate::backend::analysis::{Cfg, DomTree};
use crate::ir::*;

pub fn run(mut ir: Program) -> Program {
    for f in &mut ir.funcs {
        run_func(f);
    }
    ir
}

/// A hashable operand key (`Val` derives neither `Hash` nor `Eq`).
type VKey = (u8, i64);

fn vkey(v: Val) -> VKey {
    match v {
        Val::Reg(r) => (0, r as i64),
        Val::ImmInt(i) => (1, i),
        Val::ImmF64(b) => (2, b as i64),
    }
}

/// The value-number key for a pure instruction: its operator + types + resolved operands.
#[derive(Clone, PartialEq, Eq, Hash)]
enum Key {
    Bin(u8, u8, bool, VKey, VKey),
    Un(u8, u8, VKey),
    Cmp(u8, u8, bool, VKey, VKey),
    Cast(u8, u8, VKey),
    PtrAdd(VKey, VKey, u32),
    SlotAddr(u32, u32),
    GlobalAddr(u32, u32),
    StrAddr(u32),
    FuncAddr(String),
}

/// An 8-byte integer-class cast (`I64`/`U64`/`Ptr` → `I64`/`U64`/`Ptr`) carries the same bits;
/// both backends emit no instruction for it. Such a cast's result equals its source.
fn is_noop_cast(to: Ty, from: Ty) -> bool {
    !to.is_float() && !from.is_float() && to.size() == 8 && from.size() == 8
}

/// Follow the substitution chain to the canonical value (acyclic: each entry points at a
/// strictly-earlier dominating definition).
fn resolve(subst: &[Option<Val>], v: Val) -> Val {
    let mut cur = v;
    while let Val::Reg(r) = cur {
        match subst[r as usize] {
            Some(next) if next != cur => cur = next,
            _ => break,
        }
    }
    cur
}

/// Build the value-number key for `inst` with operands resolved through `subst`. Returns
/// `None` for instructions that are not value-numbered here. Commutative operators sort their
/// operand keys so `a op b` and `b op a` share a key.
fn make_key(inst: &Inst, subst: &[Option<Val>]) -> Option<Key> {
    let r = |v: Val| vkey(resolve(subst, v));
    let commute = |a: VKey, b: VKey| if a <= b { (a, b) } else { (b, a) };
    Some(match inst {
        Inst::Bin {
            op,
            ty,
            signed,
            lhs,
            rhs,
            ..
        } => {
            let (a, b) = (r(*lhs), r(*rhs));
            let (a, b) = match op {
                BinOp::Add | BinOp::Mul | BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor => {
                    commute(a, b)
                }
                _ => (a, b),
            };
            Key::Bin(*op as u8, *ty as u8, *signed, a, b)
        }
        Inst::Un { op, ty, src, .. } => Key::Un(*op as u8, *ty as u8, r(*src)),
        Inst::Cmp {
            op,
            ty,
            signed,
            lhs,
            rhs,
            ..
        } => {
            let (a, b) = (r(*lhs), r(*rhs));
            let (a, b) = match op {
                CmpOp::Eq | CmpOp::Ne => commute(a, b),
                _ => (a, b),
            };
            Key::Cmp(*op as u8, *ty as u8, *signed, a, b)
        }
        Inst::Cast { to, from, src, .. } => Key::Cast(*to as u8, *from as u8, r(*src)),
        Inst::PtrAdd {
            base,
            index,
            stride,
            ..
        } => Key::PtrAdd(r(*base), r(*index), *stride),
        Inst::SlotAddr { slot, off, .. } => Key::SlotAddr(*slot, *off),
        Inst::GlobalAddr { global, off, .. } => Key::GlobalAddr(*global, *off),
        Inst::StrAddr { str, .. } => Key::StrAddr(*str),
        Inst::FuncAddr { func, .. } => Key::FuncAddr(func.clone()),
        _ => return None,
    })
}

fn run_func(f: &mut Func) {
    let n = f.n_vregs as usize;
    if n == 0 || f.blocks.is_empty() {
        return;
    }
    let cfg = Cfg::new(f);
    let dom = DomTree::new(f, &cfg);

    // Dominator-tree children, for the scoped pre-order walk. Unreachable blocks have no
    // immediate dominator (`idom == u32::MAX`); they are skipped here (never visited) and the
    // final rewrite sweep leaves their dead instructions untouched.
    let mut children: Vec<Vec<BlockId>> = vec![Vec::new(); f.blocks.len()];
    for b in 0..f.blocks.len() {
        let id = b as BlockId;
        if id != f.entry && dom.idom[b] != u32::MAX {
            children[dom.idom[b] as usize].push(id);
        }
    }

    let mut subst: Vec<Option<Val>> = vec![None; n];
    let mut removed: Vec<Vec<bool>> = f
        .blocks
        .iter()
        .map(|b| vec![false; b.insts.len()])
        .collect();
    let mut avail: HashMap<Key, Val> = HashMap::new();
    // The loads still available at each block's *exit* (after its clobbers), so a block with a
    // single predecessor can inherit them: with no merge, that predecessor is the only way to
    // reach the block, and a load it left available cannot have been clobbered on the way in.
    // (A multi-predecessor block starts empty — proving availability across a merge would need
    // a full available-loads dataflow.) This extends redundant-load elimination across an
    // extended basic block, e.g. binary search's second `a[mid]` in the `else` arm.
    let mut exit_loads: Vec<HashMap<(VKey, u8), Val>> = vec![HashMap::new(); f.blocks.len()];

    // Explicit stack of (block, phase): phase 0 = process + queue children, phase 1 = pop the
    // keys this block added. The `added` lists ride alongside so the scope unwinds correctly.
    enum Step {
        Enter(BlockId),
        Leave(Vec<Key>),
    }
    let mut stack = vec![Step::Enter(f.entry)];
    while let Some(step) = stack.pop() {
        match step {
            Step::Leave(keys) => {
                for k in keys {
                    avail.remove(&k);
                }
            }
            Step::Enter(b) => {
                let bi = b as usize;
                let mut added: Vec<Key> = Vec::new();
                // Available loads: (resolved-address key, width tag) -> value. Seeded from the
                // sole predecessor's exit set (extended-basic-block forwarding), else empty.
                let mut loads: HashMap<(VKey, u8), Val> = match cfg.preds[bi].as_slice() {
                    [p] => exit_loads[*p as usize].clone(),
                    _ => HashMap::new(),
                };
                for (i, inst) in f.blocks[bi].insts.iter().enumerate() {
                    match inst {
                        Inst::Cast { dst, to, from, src } if is_noop_cast(*to, *from) => {
                            subst[*dst as usize] = Some(resolve(&subst, *src));
                            removed[bi][i] = true;
                        }
                        Inst::Mov { dst, src, .. } => {
                            subst[*dst as usize] = Some(resolve(&subst, *src));
                            removed[bi][i] = true;
                        }
                        Inst::Load { dst, ty, addr } => {
                            let k = (vkey(resolve(&subst, *addr)), *ty as u8);
                            if let Some(&v) = loads.get(&k) {
                                subst[*dst as usize] = Some(v);
                                removed[bi][i] = true;
                            } else {
                                loads.insert(k, Val::Reg(*dst));
                            }
                        }
                        Inst::Store { .. }
                        | Inst::MemCpy { .. }
                        | Inst::MemZero { .. }
                        | Inst::Call { .. }
                        | Inst::Prim { .. }
                        | Inst::TryBegin { .. }
                        | Inst::TryEnd => {
                            loads.clear();
                        }
                        _ => {
                            if let Some(key) = make_key(inst, &subst) {
                                let dst = inst.def().expect("value-numbered inst defines a vreg");
                                if let Some(&rep) = avail.get(&key) {
                                    subst[dst as usize] = Some(rep);
                                    removed[bi][i] = true;
                                } else {
                                    avail.insert(key.clone(), Val::Reg(dst));
                                    added.push(key);
                                }
                            }
                        }
                    }
                }
                exit_loads[bi] = loads;
                stack.push(Step::Leave(added));
                for &c in &children[bi] {
                    stack.push(Step::Enter(c));
                }
            }
        }
    }

    // Rewrite every surviving operand (instructions, phis, terminators) through the final
    // substitution, and drop the removed instructions.
    for (bi, b) in f.blocks.iter_mut().enumerate() {
        let mut idx = 0;
        b.insts.retain(|_| {
            let keep = !removed[bi][idx];
            idx += 1;
            keep
        });
        for inst in &mut b.insts {
            inst.for_each_use_mut(|v| *v = resolve(&subst, *v));
        }
        for phi in &mut b.phis {
            for (_, v) in &mut phi.args {
                *v = resolve(&subst, *v);
            }
        }
        b.term.for_each_use_mut(|v| *v = resolve(&subst, *v));
    }
}

#[cfg(test)]
#[path = "tests/cse.rs"]
mod tests;
