//! Tests for Kernighan-popcount idiom recognition. A canonical loop is rewritten (its header
//! bypasses the body); near-misses are left untouched.

use super::*;
use crate::layout::Layouts;

fn bin(dst: Vreg, op: BinOp, lhs: Val, rhs: Val) -> Inst {
    Inst::Bin {
        dst,
        op,
        ty: Ty::I64,
        signed: true,
        lhs,
        rhs,
    }
}

/// Build a 4-block Kernighan loop, with the body's three instructions supplied so a test can
/// perturb one for a near-miss:
///   b0 (preheader): br b1
///   b1 (header): phi x=[b0:p0, b2:xn]; phi c=[b0:0, b2:cn]; condbr(NonZero x) -> b2, b3
///   b2 (body): <body insts>; br b1
///   b3 (exit): ret c
/// Vregs: 0 = param x0; 1 = x-phi; 2 = c-phi; 3 = t; 4 = x_next; 5 = c_next.
fn kernighan(body: Vec<Inst>, exit_uses_x: bool) -> Func {
    let exit_ret = if exit_uses_x {
        Val::Reg(1)
    } else {
        Val::Reg(2)
    };
    Func {
        name: "@t".into(),
        ret: Ret::Scalar(Ty::I64),
        params: vec![Param {
            ty: ArgTy::Int(Ty::I64),
            vreg: 0,
            name: None,
        }],
        varargs: false,
        slots: vec![],
        blocks: vec![
            Block {
                id: 0,
                phis: vec![],
                insts: vec![],
                term: Term::Br(1),
            },
            Block {
                id: 1,
                phis: vec![
                    Phi {
                        dst: 1,
                        ty: Ty::I64,
                        args: vec![(0, Val::Reg(0)), (2, Val::Reg(4))],
                    },
                    Phi {
                        dst: 2,
                        ty: Ty::I64,
                        args: vec![(0, Val::ImmInt(0)), (2, Val::Reg(5))],
                    },
                ],
                insts: vec![],
                term: Term::CondBr {
                    cond: Cond::NonZero {
                        val: Val::Reg(1),
                        ty: Ty::I64,
                    },
                    t: 2,
                    f: 3,
                },
            },
            Block {
                id: 2,
                phis: vec![],
                insts: body,
                term: Term::Br(1),
            },
            Block {
                id: 3,
                phis: vec![],
                insts: vec![],
                term: Term::Ret(Some(exit_ret)),
            },
        ],
        entry: 0,
        n_vregs: 6,
    }
}

/// The canonical body: t = x-1; x_next = x & t; c_next = c+1.
fn canonical_body() -> Vec<Inst> {
    vec![
        bin(3, BinOp::Sub, Val::Reg(1), Val::ImmInt(1)),
        bin(4, BinOp::BitAnd, Val::Reg(1), Val::Reg(3)),
        bin(5, BinOp::Add, Val::Reg(2), Val::ImmInt(1)),
    ]
}

fn run(mut f: Func) -> Func {
    run_func(&mut f);
    let p = Program {
        funcs: vec![f],
        globals: vec![],
        strings: vec![],
        layouts: Layouts::default(),
    };
    assert!(
        crate::ir::verify(&p).is_empty(),
        "idiom produced invalid IR: {:?}",
        crate::ir::verify(&p)
    );
    p.funcs.into_iter().next().unwrap()
}

fn header_is_bypass(f: &Func) -> bool {
    matches!(f.blocks[1].term, Term::Br(3))
}

#[test]
fn canonical_kernighan_rewritten() {
    let f = run(kernighan(canonical_body(), false));
    assert!(
        header_is_bypass(&f),
        "the header should bypass the loop body"
    );
    assert!(
        !f.blocks[0].insts.is_empty(),
        "the preheader should gain the popcount sequence"
    );
    // The c-phi's preheader arg is now a fresh vreg (the forwarded count), not the original 0.
    let phi_c = f.blocks[1].phis.iter().find(|p| p.dst == 2).unwrap();
    let pre = phi_c.args.iter().find(|(b, _)| *b == 0).unwrap().1;
    assert!(
        matches!(pre, Val::Reg(_)),
        "c-phi preheader arg should be the forwarded count"
    );
}

#[test]
fn wrong_increment_not_rewritten() {
    // c_next = c + 2 — not a counter.
    let mut body = canonical_body();
    body[2] = bin(5, BinOp::Add, Val::Reg(2), Val::ImmInt(2));
    let f = run(kernighan(body, false));
    assert!(
        !header_is_bypass(&f),
        "a non-+1 counter must not be rewritten"
    );
}

#[test]
fn wrong_clear_op_not_rewritten() {
    // x_next = x + t  (not x & t) — not the bit-clear idiom.
    let mut body = canonical_body();
    body[1] = bin(4, BinOp::Add, Val::Reg(1), Val::Reg(3));
    let f = run(kernighan(body, false));
    assert!(
        !header_is_bypass(&f),
        "a non-AND clear must not be rewritten"
    );
}

#[test]
fn wrong_decrement_not_rewritten() {
    // t = x - 2  (not x - 1) — `x & (x-2)` is not Kernighan.
    let mut body = canonical_body();
    body[0] = bin(3, BinOp::Sub, Val::Reg(1), Val::ImmInt(2));
    let f = run(kernighan(body, false));
    assert!(!header_is_bypass(&f), "a non-(x-1) must not be rewritten");
}

#[test]
fn x_used_after_loop_not_rewritten() {
    // The exit returns x (live past the loop) — bypassing would drop its final value.
    let f = run(kernighan(canonical_body(), true));
    assert!(
        !header_is_bypass(&f),
        "if x escapes the loop it must not be rewritten"
    );
}

#[test]
fn extra_body_inst_not_rewritten() {
    // An extra (effect-free) instruction in the body — shape no longer exact.
    let mut body = canonical_body();
    body.push(bin(7, BinOp::Add, Val::Reg(2), Val::Reg(3)));
    let mut f = kernighan(body, false);
    f.n_vregs = 8;
    let f = run(f);
    assert!(
        !header_is_bypass(&f),
        "an extra body instruction must block the rewrite"
    );
}
