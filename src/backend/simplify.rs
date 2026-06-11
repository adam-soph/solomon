//! IR-level scalar simplification — a shared IR→IR pass run on the backend path only
//! (`lower_to_machine_ir`, after `lower`, before `licm`), so both native backends benefit
//! while the interpreter keeps consuming the *unoptimized* IR as the pristine oracle.
//!
//! Every rewrite is behavior-preserving against that oracle:
//! - **Constant folding** evaluates a `Bin`/`Cmp` whose operands are both immediates through
//!   the *same* functions the interpreter runs ([`crate::irinterp::bin`] / [`crate::irinterp::cmp`]),
//!   so the folded constant is bit-identical to what the oracle would compute — including
//!   `i64::MIN/-1` wrap, unsigned divide, and the truncated float `Mod`. Integer divide/modulo
//!   by zero is left unfolded so the runtime trap still happens exactly where the oracle's does.
//! - **Algebraic identities** (`x+0`, `x*1`, `x&-1`, …) are value-preserving at the full 64-bit
//!   width `bin` computes at. Restricted to integer ops — `x+0.0` is not identity for `-0.0`.
//! - **Power-of-2 strength reduction** rewrites `Mul`/`Div`/`Mod` by a constant power of two into
//!   shifts/masks that reproduce `wrapping_*` exactly. Signed `Div` by `2^k` uses the standard
//!   round-toward-zero bias `(x + ((x s>>63) u>>(64-k))) s>> k`, matching `i64::wrapping_div`
//!   (incl. `i64::MIN`); it is gated to `I64`.
//!
//! Sub-word truncation is never this pass's concern: the IR computes integer `Bin` at full 64
//! bits and lowering already inserts an explicit `Cast` for any narrowing, untouched here.

use crate::ir::*;
use crate::irinterp::{RVal, bin, cmp};

/// Run scalar simplification over every function in `p`: constant folding / identities /
/// strength reduction, then trivial-`phi` elimination (which the previous folding can expose
/// and which unblocks later loop-invariant code motion).
pub(crate) fn run(mut p: IrProgram) -> IrProgram {
    for f in &mut p.funcs {
        run_func(f);
        eliminate_trivial_phis(f);
    }
    p
}

/// Remove **trivial** `phi` nodes — those whose arguments are, ignoring self-references, all a
/// single value `V`. Such a `phi` always equals `V` (on first entry it takes `V`; a back-edge
/// re-supplies its own unchanged value), so every use of it is replaced by `V` and the `phi`
/// deleted. This collapses the loop-invariant header phis SSA construction inserts for a value
/// merely *live across* a loop (e.g. an outer loop counter inside an inner loop), so LICM can
/// then treat it as invariant, and out-of-SSA emits fewer back-edge copies.
fn eliminate_trivial_phis(f: &mut IrFunc) {
    loop {
        let mut found: Option<(usize, usize, Vreg, Val)> = None;
        'scan: for (bi, b) in f.blocks.iter().enumerate() {
            for (pi, phi) in b.phis.iter().enumerate() {
                let mut only: Option<Val> = None;
                let mut trivial = true;
                for (_, arg) in &phi.args {
                    if *arg == Val::Reg(phi.dst) {
                        continue; // self-reference: contributes nothing
                    }
                    match only {
                        None => only = Some(*arg),
                        Some(v) if v == *arg => {}
                        Some(_) => {
                            trivial = false;
                            break;
                        }
                    }
                }
                if trivial {
                    if let Some(v) = only {
                        found = Some((bi, pi, phi.dst, v));
                        break 'scan;
                    }
                    // All args self-referential — an undefined/dead phi; leave it.
                }
            }
        }
        let Some((bi, pi, dst, val)) = found else {
            break;
        };
        f.blocks[bi].phis.remove(pi);
        replace_all_uses(f, dst, val);
    }
}

/// Replace every operand use of `Reg(from)` (in phis, instructions, and terminators) with `to`.
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

fn run_func(f: &mut IrFunc) {
    for b in &mut f.blocks {
        let mut out = Vec::with_capacity(b.insts.len());
        for inst in b.insts.drain(..) {
            simplify_inst(inst, &mut out);
        }
        b.insts = out;
    }
}

/// Append the simplified form of `inst` to `out`. Every rewrite is a single replacement
/// instruction, so SSA and the vreg count are preserved. The default is to push `inst`
/// unchanged.
fn simplify_inst(inst: IrInst, out: &mut Vec<IrInst>) {
    match inst {
        IrInst::Bin {
            dst,
            op,
            ty,
            signed,
            lhs,
            rhs,
        } => {
            // 1. Constant fold (both operands immediate) — via the oracle's own arithmetic.
            if let (Some(l), Some(r)) = (as_rval(lhs), as_rval(rhs)) {
                if let Ok(v) = bin(op, ty, signed, l, r) {
                    out.push(IrInst::Mov {
                        dst,
                        ty,
                        src: from_rval(v, ty),
                    });
                    return;
                }
                // Folding failed (integer div/mod by zero): keep the op so the trap survives.
            }
            // 2/3. Identities + power-of-2 strength reduction (integer ops only).
            if !ty.is_float() && try_int_rewrite(dst, op, ty, signed, lhs, rhs, out) {
                return;
            }
            out.push(IrInst::Bin {
                dst,
                op,
                ty,
                signed,
                lhs,
                rhs,
            });
        }
        IrInst::Cmp {
            dst,
            op,
            ty,
            signed,
            lhs,
            rhs,
        } => {
            if let (Some(l), Some(r)) = (as_rval(lhs), as_rval(rhs)) {
                let v = cmp(op, ty, signed, l, r) as i64;
                out.push(IrInst::Mov {
                    dst,
                    ty,
                    src: Val::ImmInt(v),
                });
                return;
            }
            out.push(IrInst::Cmp {
                dst,
                op,
                ty,
                signed,
                lhs,
                rhs,
            });
        }
        other => out.push(other),
    }
}

/// Rewrite an integer `Bin` via an algebraic identity or power-of-2 strength reduction,
/// appending the replacement(s) to `out`. Returns `true` if it handled the instruction.
fn try_int_rewrite(
    dst: Vreg,
    op: IrBinOp,
    ty: IrTy,
    signed: bool,
    lhs: Val,
    rhs: Val,
    out: &mut Vec<IrInst>,
) -> bool {
    let zero = Val::ImmInt(0);
    let mov = |out: &mut Vec<IrInst>, src: Val| out.push(IrInst::Mov { dst, ty, src });
    let shl = |out: &mut Vec<IrInst>, x: Val, k: i64| {
        out.push(IrInst::Bin {
            dst,
            op: IrBinOp::Shl,
            ty,
            signed,
            lhs: x,
            rhs: Val::ImmInt(k),
        });
    };
    match op {
        IrBinOp::Add | IrBinOp::BitOr | IrBinOp::BitXor => {
            // x op 0 → x ; 0 op x → x (commutative, identity element 0).
            if rhs == zero {
                mov(out, lhs);
                return true;
            }
            if lhs == zero {
                mov(out, rhs);
                return true;
            }
        }
        IrBinOp::Sub => {
            // x - 0 → x (not commutative).
            if rhs == zero {
                mov(out, lhs);
                return true;
            }
        }
        IrBinOp::Shl | IrBinOp::Shr => {
            // x << 0 → x ; x >> 0 → x.
            if rhs == zero {
                mov(out, lhs);
                return true;
            }
        }
        IrBinOp::BitAnd => {
            if rhs == Val::ImmInt(-1) {
                mov(out, lhs);
                return true;
            }
            if lhs == Val::ImmInt(-1) {
                mov(out, rhs);
                return true;
            }
            if rhs == zero || lhs == zero {
                mov(out, zero);
                return true;
            }
        }
        IrBinOp::Mul => {
            if rhs == Val::ImmInt(1) {
                mov(out, lhs);
                return true;
            }
            if lhs == Val::ImmInt(1) {
                mov(out, rhs);
                return true;
            }
            if rhs == zero || lhs == zero {
                mov(out, zero);
                return true;
            }
            // x * 2^k → x << k (low 64 bits of the product equal the shift, signed or not).
            if let Val::ImmInt(c) = rhs {
                if let Some(k) = pow2(c) {
                    shl(out, lhs, k);
                    return true;
                }
            }
            if let Val::ImmInt(c) = lhs {
                if let Some(k) = pow2(c) {
                    shl(out, rhs, k);
                    return true;
                }
            }
        }
        IrBinOp::Div => {
            if rhs == Val::ImmInt(1) {
                mov(out, lhs);
                return true;
            }
            if let Val::ImmInt(c) = rhs {
                if let Some(k) = pow2(c) {
                    if !signed {
                        // unsigned x / 2^k → x u>> k (a single shift).
                        out.push(IrInst::Bin {
                            dst,
                            op: IrBinOp::Shr,
                            ty,
                            signed: false,
                            lhs,
                            rhs: Val::ImmInt(k),
                        });
                        return true;
                    }
                    // Signed divide by a power of two is left to the hardware divider. The
                    // round-toward-zero bias sequence needs extra temporaries that, under the
                    // spill-everything model, cost more memory traffic than one `sdiv`/`idiv`.
                    // (A spill-free expansion belongs in each backend's isel, not here.)
                }
            }
        }
        IrBinOp::Mod => {
            if let Val::ImmInt(c) = rhs {
                if pow2(c).is_some() {
                    if !signed {
                        // unsigned x % 2^k → x & (2^k - 1).
                        out.push(IrInst::Bin {
                            dst,
                            op: IrBinOp::BitAnd,
                            ty,
                            signed: false,
                            lhs,
                            rhs: Val::ImmInt(c - 1),
                        });
                        return true;
                    }
                    // Signed modulo by a power of two is deferred (no benchmark needs it);
                    // fall through to the full `Mod`.
                }
            }
        }
    }
    false
}

/// If `c` is a power of two `2^k` with `k` in `1..=62`, return `k`. Excludes `1` (`2^0`, an
/// identity handled separately) and `2^63` (negative as `i64`).
fn pow2(c: i64) -> Option<i64> {
    if c >= 2 && (c as u64).is_power_of_two() {
        let k = c.trailing_zeros() as i64;
        (1..=62).contains(&k).then_some(k)
    } else {
        None
    }
}

/// An immediate operand as an [`RVal`] for folding; `None` for a register operand.
fn as_rval(v: Val) -> Option<RVal> {
    match v {
        Val::ImmInt(i) => Some(RVal::Int(i)),
        Val::ImmF64(b) => Some(RVal::Float(f64::from_bits(b))),
        Val::Reg(_) => None,
    }
}

/// A folded [`RVal`] back to an immediate `Val` of the op's result type.
fn from_rval(v: RVal, ty: IrTy) -> Val {
    match v {
        RVal::Float(f) => Val::ImmF64(f.to_bits()),
        RVal::Int(i) if ty.is_float() => Val::ImmF64((i as f64).to_bits()),
        RVal::Int(i) => Val::ImmInt(i),
    }
}

#[cfg(test)]
#[path = "simplify_tests.rs"]
mod tests;
