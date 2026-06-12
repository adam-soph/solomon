//! Loop-idiom recognition on the shared SSA IR (backend path only — the interpreter keeps the
//! unoptimized IR as the oracle, so it just runs the loop). Currently one idiom: **Kernighan's
//! population count**, `while (x) { x = x & (x - 1); c = c + 1; }`, which clears the lowest set
//! bit each iteration and so runs `popcount(x0)` times. The whole loop is replaced by a
//! constant-time bit-twiddling popcount of the initial value, computed in the preheader, with
//! the running count forwarded to the exit.
//!
//! The popcount itself is emitted as the standard SWAR sequence of ordinary integer `Bin`s, so
//! no new IR op or backend support is needed — every target already lowers it, and the
//! interpreter never sees it (it runs the original loop). The rewrite leaves the now-bypassed
//! loop body unreachable; DCE and the backends' reachable-block walk drop the dead remainder.
//!
//! The matcher is deliberately strict (exact instruction shape, operand wiring, and `x` dead
//! past the loop), so it fires only on the genuine idiom; any near-miss is simply left alone.

use crate::backend::analysis::{Cfg, DomTree, LoopForest};
use crate::ir::*;

pub fn run(mut ir: Program) -> Program {
    for f in &mut ir.funcs {
        run_func(f);
    }
    ir
}

/// Expand every [`UnOp::Popcount`] into the portable SWAR `Bin` sequence, for backends with
/// no population-count instruction (x86, where `popcnt` is not in the guaranteed baseline). The
/// arm64 backend skips this and lowers the op to `cnt` directly. Returns a fresh program.
pub(crate) fn expand_popcount(ir: &Program) -> Program {
    let mut out = ir.clone();
    for f in &mut out.funcs {
        let mut next = f.n_vregs;
        for b in &mut f.blocks {
            let mut new_insts = Vec::with_capacity(b.insts.len());
            for inst in b.insts.drain(..) {
                if let Inst::Un {
                    dst,
                    op: UnOp::Popcount,
                    src,
                    ..
                } = inst
                {
                    let r = emit_popcount(&mut new_insts, &mut next, src);
                    new_insts.push(Inst::Mov {
                        dst,
                        ty: Ty::I64,
                        src: r,
                    });
                } else {
                    new_insts.push(inst);
                }
            }
            b.insts = new_insts;
        }
        f.n_vregs = next;
    }
    out
}

/// `dst` of a `phi`/instruction, the val it carries from a given predecessor block, etc. — small
/// readers kept local to the matcher.
fn phi_arg_from(phi: &Phi, block: BlockId) -> Option<Val> {
    phi.args.iter().find(|(b, _)| *b == block).map(|(_, v)| *v)
}

fn phi_arg_not_from(phi: &Phi, block: BlockId) -> Option<(BlockId, Val)> {
    let others: Vec<_> = phi.args.iter().filter(|(b, _)| *b != block).collect();
    match others.as_slice() {
        [(b, v)] => Some((*b, *v)),
        _ => None,
    }
}

/// Append the SWAR 64-bit population count of `x` to `insts`, allocating fresh vregs from
/// `next`. Returns the vreg holding the count. All ops are wrapping `I64` integer ops the
/// backends already support; shifts are logical (count every bit regardless of sign). Used by
/// the x86 backend to expand [`UnOp::Popcount`] (which lacks a guaranteed instruction there).
pub(crate) fn emit_popcount(insts: &mut Vec<Inst>, next: &mut u32, x: Val) -> Val {
    const M1: i64 = 0x5555_5555_5555_5555;
    const M2: i64 = 0x3333_3333_3333_3333;
    const M4: i64 = 0x0f0f_0f0f_0f0f_0f0f;
    const H01: i64 = 0x0101_0101_0101_0101;
    let mut fresh = || {
        let v = *next;
        *next += 1;
        v
    };
    let bin = |dst: Vreg, op: BinOp, lhs: Val, rhs: Val| Inst::Bin {
        dst,
        op,
        ty: Ty::I64,
        signed: false,
        lhs,
        rhs,
    };
    // v = x - ((x >> 1) & M1)
    let (s1, a1, v1) = (fresh(), fresh(), fresh());
    insts.push(bin(s1, BinOp::Shr, x, Val::ImmInt(1)));
    insts.push(bin(a1, BinOp::BitAnd, Val::Reg(s1), Val::ImmInt(M1)));
    insts.push(bin(v1, BinOp::Sub, x, Val::Reg(a1)));
    // v = (v & M2) + ((v >> 2) & M2)
    let (lo, s2, hi, v2) = (fresh(), fresh(), fresh(), fresh());
    insts.push(bin(lo, BinOp::BitAnd, Val::Reg(v1), Val::ImmInt(M2)));
    insts.push(bin(s2, BinOp::Shr, Val::Reg(v1), Val::ImmInt(2)));
    insts.push(bin(hi, BinOp::BitAnd, Val::Reg(s2), Val::ImmInt(M2)));
    insts.push(bin(v2, BinOp::Add, Val::Reg(lo), Val::Reg(hi)));
    // v = (v + (v >> 4)) & M4
    let (s4, sum, v3) = (fresh(), fresh(), fresh());
    insts.push(bin(s4, BinOp::Shr, Val::Reg(v2), Val::ImmInt(4)));
    insts.push(bin(sum, BinOp::Add, Val::Reg(v2), Val::Reg(s4)));
    insts.push(bin(v3, BinOp::BitAnd, Val::Reg(sum), Val::ImmInt(M4)));
    // count = (v * H01) >> 56
    let (mul, count) = (fresh(), fresh());
    insts.push(bin(mul, BinOp::Mul, Val::Reg(v3), Val::ImmInt(H01)));
    insts.push(bin(count, BinOp::Shr, Val::Reg(mul), Val::ImmInt(56)));
    Val::Reg(count)
}

/// Whether `v` is read by `inst` (operand equality on `Val::Reg(v)`).
fn inst_reads(inst: &Inst, v: Vreg) -> bool {
    let mut found = false;
    inst.for_each_use(|u| {
        if u == Val::Reg(v) {
            found = true;
        }
    });
    found
}

fn term_reads(term: &Term, v: Vreg) -> bool {
    let mut found = false;
    term.for_each_use(|u| {
        if u == Val::Reg(v) {
            found = true;
        }
    });
    found
}

fn run_func(f: &mut Func) {
    if f.blocks.len() < 3 {
        return;
    }
    let cfg = Cfg::new(f);
    let dom = DomTree::new(f, &cfg);
    let loops = LoopForest::new(f, &cfg, &dom);

    // Collect rewrites first (the match only reads `f`), then apply them.
    struct Rewrite {
        header: BlockId,
        body: BlockId,
        exit: BlockId,
        preheader: BlockId,
        x0: Val,
        c0: Val,
        phi_c_dst: Vreg,
    }
    let mut rewrites: Vec<Rewrite> = Vec::new();

    for lp in &loops.loops {
        // A single-block-body loop: one latch, body = {header, latch}.
        let [body] = lp.latches.as_slice() else {
            continue;
        };
        let body = *body;
        let header = lp.header;
        if body == header || lp.body.len() != 2 || !lp.body.contains(&body) {
            continue;
        }
        let hblk = &f.blocks[header as usize];
        // Header: exactly two phis and a `CondBr(NonZero x) -> body, exit`.
        if !hblk.insts.is_empty() || hblk.phis.len() != 2 {
            continue;
        }
        let Term::CondBr {
            cond: Cond::NonZero {
                val: Val::Reg(xv), ..
            },
            t,
            f: fexit,
        } = hblk.term
        else {
            continue;
        };
        // The true edge must enter the body; the false edge is the exit.
        if t != body {
            continue;
        }
        let exit = fexit;
        // Identify the x-phi (the loop condition) and the c-phi (the other).
        let (phi_x, phi_c) = match (&hblk.phis[0], &hblk.phis[1]) {
            (a, b) if a.dst == xv => (a, b),
            (a, b) if b.dst == xv => (b, a),
            _ => continue,
        };
        // Body must be exactly: t = x - 1; x_next = x & t; c_next = c + 1; Br header.
        let bblk = &f.blocks[body as usize];
        if !bblk.phis.is_empty()
            || bblk.insts.len() != 3
            || !matches!(bblk.term, Term::Br(h) if h == header)
        {
            continue;
        }
        let x = phi_x.dst;
        let c = phi_c.dst;
        // inst 0: t = x - 1
        let Inst::Bin {
            dst: tdst,
            op: BinOp::Sub,
            lhs: Val::Reg(tl),
            rhs: Val::ImmInt(1),
            ..
        } = bblk.insts[0]
        else {
            continue;
        };
        if tl != x {
            continue;
        }
        // inst 1: x_next = x & t  (either operand order)
        let Inst::Bin {
            dst: xn,
            op: BinOp::BitAnd,
            lhs: Val::Reg(al),
            rhs: Val::Reg(ar),
            ..
        } = bblk.insts[1]
        else {
            continue;
        };
        if !((al == x && ar == tdst) || (al == tdst && ar == x)) {
            continue;
        }
        // inst 2: c_next = c + 1  (either operand order)
        let Inst::Bin {
            dst: cn,
            op: BinOp::Add,
            lhs,
            rhs,
            ..
        } = bblk.insts[2]
        else {
            continue;
        };
        let cn_ok = matches!((lhs, rhs), (Val::Reg(r), Val::ImmInt(1)) | (Val::ImmInt(1), Val::Reg(r)) if r == c);
        if !cn_ok {
            continue;
        }
        // The phis must take x_next / c_next from the body, and a preheader value from the
        // other (sole) predecessor.
        if phi_arg_from(phi_x, body) != Some(Val::Reg(xn))
            || phi_arg_from(phi_c, body) != Some(Val::Reg(cn))
        {
            continue;
        }
        let (Some((pre_x, x0)), Some((pre_c, c0))) =
            (phi_arg_not_from(phi_x, body), phi_arg_not_from(phi_c, body))
        else {
            continue;
        };
        if pre_x != pre_c {
            continue;
        }
        let preheader = pre_x;
        // `x` must be dead past the loop: used only by the header's CondBr and the body. If any
        // other instruction or terminator (anywhere) reads it, bail.
        let x_escapes = f.blocks.iter().enumerate().any(|(bi, blk)| {
            let in_body = bi as BlockId == body;
            let in_header = bi as BlockId == header;
            blk.phis
                .iter()
                .any(|p| p.dst != x && p.args.iter().any(|(_, v)| *v == Val::Reg(x)))
                || (!in_body && blk.insts.iter().any(|i| inst_reads(i, x)))
                || (!in_header && term_reads(&blk.term, x))
        });
        if x_escapes {
            continue;
        }

        rewrites.push(Rewrite {
            header,
            body,
            exit,
            preheader,
            x0,
            c0,
            phi_c_dst: c,
        });
    }

    if rewrites.is_empty() {
        return;
    }
    let mut next = f.n_vregs;
    for rw in rewrites {
        // Preheader: pc = popcount(x0), then forward c0 + pc. The popcount is a single `Un` op
        // the backends lower optimally (arm64 `cnt`; x86 expands it to the SWAR sequence below).
        // `simplify` already ran (it precedes idiom in the pipeline), so fold the `c0 + pc` here:
        // when c0 is 0 (a counter starting at 0 — the usual case) forward `pc` directly rather
        // than emitting a dead `0 + pc` add.
        let pc = next;
        next += 1;
        let mut tail = vec![Inst::Un {
            dst: pc,
            op: UnOp::Popcount,
            ty: Ty::I64,
            src: rw.x0,
        }];
        let forwarded = if rw.c0 == Val::ImmInt(0) {
            Val::Reg(pc)
        } else {
            let c_final = next;
            next += 1;
            tail.push(Inst::Bin {
                dst: c_final,
                op: BinOp::Add,
                ty: Ty::I64,
                signed: true,
                lhs: rw.c0,
                rhs: Val::Reg(pc),
            });
            Val::Reg(c_final)
        };
        f.blocks[rw.preheader as usize].insts.extend(tail);
        // Forward the final count into the c-phi's preheader arg, and bypass the loop.
        let phi_c = f.blocks[rw.header as usize]
            .phis
            .iter_mut()
            .find(|p| p.dst == rw.phi_c_dst)
            .unwrap();
        for (b, v) in &mut phi_c.args {
            if *b == rw.preheader {
                *v = forwarded;
            }
        }
        f.blocks[rw.header as usize].term = Term::Br(rw.exit);
        let _ = rw.body;
    }
    f.n_vregs = next;
}

#[cfg(test)]
#[path = "tests/idiom.rs"]
mod tests;
