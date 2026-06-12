//! Tests for the mark-sweep DCE pass: dead pure chains are swept, effectful instructions and
//! loads are preserved, and dead `phi` cycles are collected.

use super::*;
use crate::layout::Layouts;

fn func(n_vregs: u32, blocks: Vec<Block>) -> Func {
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
        blocks,
        entry: 0,
        n_vregs,
    }
}

fn block(id: BlockId, phis: Vec<Phi>, insts: Vec<Inst>, term: Term) -> Block {
    Block {
        id,
        phis,
        insts,
        term,
    }
}

fn add(dst: Vreg, lhs: Val, rhs: Val) -> Inst {
    Inst::Bin {
        dst,
        op: BinOp::Add,
        ty: Ty::I64,
        signed: true,
        lhs,
        rhs,
    }
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
        "DCE produced invalid IR: {:?}",
        crate::ir::verify(&p)
    );
    p.funcs.into_iter().next().unwrap()
}

/// A pure chain feeding only a dead value is fully swept (iterated: removing the consumer frees
/// its producer).
#[test]
fn dead_pure_chain_swept() {
    // v1 = ptr+; v2 = v1 + 1 (both dead); ret v0
    let f = run(func(
        3,
        vec![block(
            0,
            vec![],
            vec![
                Inst::PtrAdd {
                    dst: 1,
                    base: Val::Reg(0),
                    index: Val::ImmInt(1),
                    stride: 8,
                },
                add(2, Val::Reg(1), Val::ImmInt(1)),
            ],
            Term::Ret(Some(Val::Reg(0))),
        )],
    ));
    assert!(
        f.blocks[0].insts.is_empty(),
        "the dead PtrAdd→Add chain should be swept"
    );
}

/// A pure value that *is* used survives.
#[test]
fn live_value_kept() {
    let f = run(func(
        2,
        vec![block(
            0,
            vec![],
            vec![add(1, Val::Reg(0), Val::ImmInt(1))],
            Term::Ret(Some(Val::Reg(1))),
        )],
    ));
    assert_eq!(f.blocks[0].insts.len(), 1, "a used Bin must be kept");
}

/// A `Load` with a dead result is NOT removed (it might fault); a `Store` is always kept.
#[test]
fn load_and_store_kept() {
    // v1 = load v0 (dead); store v0 <- 5; ret v0
    let f = run(func(
        2,
        vec![block(
            0,
            vec![],
            vec![
                Inst::Load {
                    dst: 1,
                    ty: Ty::I64,
                    addr: Val::Reg(0),
                },
                Inst::Store {
                    ty: Ty::I64,
                    addr: Val::Reg(0),
                    val: Val::ImmInt(5),
                },
            ],
            Term::Ret(Some(Val::Reg(0))),
        )],
    ));
    assert_eq!(
        f.blocks[0].insts.len(),
        2,
        "Load and Store must both survive DCE"
    );
}

/// A `Call` (effectful) and its operands survive even if the call result is unused.
#[test]
fn call_kept() {
    // v2 = v0 + 1; call f(v2) [no dst]; ret v0   — v2 is used by the call, so both survive.
    let f = run(func(
        3,
        vec![block(
            0,
            vec![],
            vec![
                add(2, Val::Reg(0), Val::ImmInt(1)),
                Inst::Call {
                    dst: None,
                    ret: Ret::Void,
                    callee: Callee::Direct("f".into()),
                    args: vec![ArgVal {
                        ty: ArgTy::Int(Ty::I64),
                        val: Val::Reg(2),
                    }],
                    sret: None,
                    varargs: VarargInfo::default(),
                },
            ],
            Term::Ret(Some(Val::Reg(0))),
        )],
    ));
    assert_eq!(
        f.blocks[0].insts.len(),
        2,
        "the call and the value it consumes must survive"
    );
}

/// Two mutually-referencing phis that nothing else uses are both swept (the mark-sweep never
/// reaches them from a root — a naive use-count fixpoint would keep them alive forever).
#[test]
fn dead_phi_cycle_removed() {
    // b0: br b1
    // b1: phi v1 = [b0: 0, b1: v2]; phi v2 = [b0: 0, b1: v1]; br b1
    //     (an infinite loop, but structurally valid; v1/v2 feed only each other)
    let f = run(func(
        3,
        vec![
            block(0, vec![], vec![], Term::Br(1)),
            block(
                1,
                vec![
                    Phi {
                        dst: 1,
                        ty: Ty::I64,
                        args: vec![(0, Val::ImmInt(0)), (1, Val::Reg(2))],
                    },
                    Phi {
                        dst: 2,
                        ty: Ty::I64,
                        args: vec![(0, Val::ImmInt(0)), (1, Val::Reg(1))],
                    },
                ],
                vec![],
                Term::Br(1),
            ),
        ],
    ));
    assert!(
        f.blocks[1].phis.is_empty(),
        "a dead phi cycle should be fully swept"
    );
}

/// A phi whose result is live is kept.
#[test]
fn live_phi_kept() {
    // b0: br b1 ; b1: phi v1 = [b0: v0]; ret v1
    let f = run(func(
        2,
        vec![
            block(0, vec![], vec![], Term::Br(1)),
            block(
                1,
                vec![Phi {
                    dst: 1,
                    ty: Ty::I64,
                    args: vec![(0, Val::Reg(0))],
                }],
                vec![],
                Term::Ret(Some(Val::Reg(1))),
            ),
        ],
    ));
    assert_eq!(f.blocks[1].phis.len(), 1, "a used phi must be kept");
}
