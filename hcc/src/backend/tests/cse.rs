//! Tests for the CSE pass: value numbering, no-op cast elimination, and block-local
//! redundant-load elimination. Fixtures are hand-built SSA `Func`s; assertions read the
//! rewritten instruction list.

use super::*;
use crate::layout::Layouts;

fn func(n_vregs: u32, blocks: Vec<Block>) -> Func {
    Func {
        name: "@t".into(),
        ret: Ret::Scalar(Ty::I64),
        params: vec![
            Param {
                ty: ArgTy::Int(Ty::I64),
                vreg: 0,
                name: None,
            },
            Param {
                ty: ArgTy::Int(Ty::I64),
                vreg: 1,
                name: None,
            },
        ],
        varargs: false,
        slots: vec![],
        blocks,
        entry: 0,
        n_vregs,
    }
}

fn block(id: BlockId, insts: Vec<Inst>, term: Term) -> Block {
    Block {
        id,
        phis: vec![],
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
        "CSE produced invalid IR: {:?}",
        crate::ir::verify(&p)
    );
    p.funcs.into_iter().next().unwrap()
}

/// All instructions of block 0, for assertions.
fn insts0(f: &Func) -> &[Inst] {
    &f.blocks[0].insts
}

/// The two operands of a `Bin` at `block0.insts[i]`.
fn bin_operands(f: &Func, i: usize) -> (Val, Val) {
    bin_operands_at(f, 0, i)
}

/// The two operands of a `Bin` at `block[b].insts[i]`.
fn bin_operands_at(f: &Func, b: usize, i: usize) -> (Val, Val) {
    match &f.blocks[b].insts[i] {
        Inst::Bin { lhs, rhs, .. } => (*lhs, *rhs),
        other => panic!("expected Bin, got {other:?}"),
    }
}

/// Two identical `Bin`s in the same block collapse to one; the second's uses redirect to the
/// first's result.
#[test]
fn same_block_duplicate_merged() {
    // v2 = v0 + v1; v3 = v0 + v1; v4 = v2 + v3; ret v4
    let f = run(func(
        5,
        vec![block(
            0,
            vec![
                add(2, Val::Reg(0), Val::Reg(1)),
                add(3, Val::Reg(0), Val::Reg(1)),
                add(4, Val::Reg(2), Val::Reg(3)),
            ],
            Term::Ret(Some(Val::Reg(4))),
        )],
    ));
    // The duplicate (v3) is gone: 2 insts left (v2, v4).
    assert_eq!(insts0(&f).len(), 2, "duplicate Bin should be removed");
    // v4 = v2 + v2 (the v3 operand rewritten to v2).
    assert_eq!(bin_operands(&f, 1), (Val::Reg(2), Val::Reg(2)));
}

/// A duplicate in a dominated block reuses the value from the dominating block.
#[test]
fn dominating_block_dup_merged() {
    // b0: v2 = v0 + v1; br b1
    // b1: v3 = v0 + v1; ret v3
    let f = run(func(
        4,
        vec![
            block(0, vec![add(2, Val::Reg(0), Val::Reg(1))], Term::Br(1)),
            block(
                1,
                vec![add(3, Val::Reg(0), Val::Reg(1))],
                Term::Ret(Some(Val::Reg(3))),
            ),
        ],
    ));
    assert!(
        f.blocks[1].insts.is_empty(),
        "dominated dup should be removed"
    );
    assert!(
        matches!(f.blocks[1].term, Term::Ret(Some(Val::Reg(2)))),
        "use redirected to dominating def, got {:?}",
        f.blocks[1].term
    );
}

/// A value computed only in one branch is NOT available in a sibling branch (scope pop).
#[test]
fn sibling_blocks_not_merged() {
    // b0: condbr -> b1, b2
    // b1: v2 = v0 + v1; ret v2
    // b2: v3 = v0 + v1; ret v3   (must NOT be removed — b1 does not dominate b2)
    let f = run(func(
        4,
        vec![
            block(
                0,
                vec![],
                Term::CondBr {
                    cond: Cond::NonZero {
                        val: Val::Reg(0),
                        ty: Ty::I64,
                    },
                    t: 1,
                    f: 2,
                },
            ),
            block(
                1,
                vec![add(2, Val::Reg(0), Val::Reg(1))],
                Term::Ret(Some(Val::Reg(2))),
            ),
            block(
                2,
                vec![add(3, Val::Reg(0), Val::Reg(1))],
                Term::Ret(Some(Val::Reg(3))),
            ),
        ],
    ));
    assert_eq!(
        f.blocks[2].insts.len(),
        1,
        "sibling-branch computation must survive"
    );
}

/// `a + b` and `b + a` share a value number (commutative canonicalization).
#[test]
fn commutative_operands_merged() {
    // v2 = v0 + v1; v3 = v1 + v0; v4 = v2 + v3; ret v4
    let f = run(func(
        5,
        vec![block(
            0,
            vec![
                add(2, Val::Reg(0), Val::Reg(1)),
                add(3, Val::Reg(1), Val::Reg(0)),
                add(4, Val::Reg(2), Val::Reg(3)),
            ],
            Term::Ret(Some(Val::Reg(4))),
        )],
    ));
    assert_eq!(insts0(&f).len(), 2, "b+a should merge with a+b");
    assert_eq!(bin_operands(&f, 1), (Val::Reg(2), Val::Reg(2)));
}

/// An 8-byte integer reinterpret cast is replaced by its source everywhere.
#[test]
fn noop_cast_eliminated() {
    // v2 = (U64) v0 ; v3 = v2 + v1 ; ret v3   →   v2 gone, v3 = v0 + v1
    let f = run(func(
        4,
        vec![block(
            0,
            vec![
                Inst::Cast {
                    dst: 2,
                    to: Ty::U64,
                    from: Ty::I64,
                    src: Val::Reg(0),
                },
                add(3, Val::Reg(2), Val::Reg(1)),
            ],
            Term::Ret(Some(Val::Reg(3))),
        )],
    ));
    assert_eq!(insts0(&f).len(), 1, "no-op cast should be removed");
    assert_eq!(
        bin_operands(&f, 0),
        (Val::Reg(0), Val::Reg(1)),
        "cast source propagated"
    );
}

/// A narrowing cast (8 → 1 byte) is NOT a no-op and must survive.
#[test]
fn narrowing_cast_kept() {
    let f = run(func(
        3,
        vec![block(
            0,
            vec![Inst::Cast {
                dst: 2,
                to: Ty::U8,
                from: Ty::I64,
                src: Val::Reg(0),
            }],
            Term::Ret(Some(Val::Reg(2))),
        )],
    ));
    assert_eq!(insts0(&f).len(), 1, "narrowing cast must be kept");
}

fn load(dst: Vreg, addr: Val) -> Inst {
    Inst::Load {
        dst,
        ty: Ty::I64,
        addr,
    }
}

/// Two loads from the same address with nothing in between reuse the first result.
#[test]
fn redundant_load_eliminated() {
    // v3 = ptr v0; v2 = load v3; v4 = load v3; v5 = v2 + v4; ret v5
    let f = run(func(
        6,
        vec![block(
            0,
            vec![
                load(2, Val::Reg(0)),
                load(4, Val::Reg(0)),
                add(5, Val::Reg(2), Val::Reg(4)),
            ],
            Term::Ret(Some(Val::Reg(5))),
        )],
    ));
    // Second load removed: load + add survive.
    assert_eq!(insts0(&f).len(), 2, "redundant load should be removed");
    assert_eq!(
        bin_operands(&f, 1),
        (Val::Reg(2), Val::Reg(2)),
        "second load redirected to first"
    );
}

/// A store between two loads invalidates the cached value — the second load survives.
#[test]
fn load_store_load_kept() {
    let f = run(func(
        6,
        vec![block(
            0,
            vec![
                load(2, Val::Reg(0)),
                Inst::Store {
                    ty: Ty::I64,
                    addr: Val::Reg(1),
                    val: Val::Reg(2),
                },
                load(4, Val::Reg(0)),
                add(5, Val::Reg(2), Val::Reg(4)),
            ],
            Term::Ret(Some(Val::Reg(5))),
        )],
    ));
    assert_eq!(
        insts0(&f).len(),
        4,
        "a store between loads must block load elimination"
    );
}

/// A call between two loads invalidates the cache (the callee may write memory).
#[test]
fn load_call_load_kept() {
    let f = run(func(
        6,
        vec![block(
            0,
            vec![
                load(2, Val::Reg(0)),
                Inst::Call {
                    dst: None,
                    ret: Ret::Void,
                    callee: Callee::Direct("f".into()),
                    args: vec![],
                    sret: None,
                    varargs: VarargInfo::default(),
                },
                load(4, Val::Reg(0)),
                add(5, Val::Reg(2), Val::Reg(4)),
            ],
            Term::Ret(Some(Val::Reg(5))),
        )],
    ));
    assert_eq!(
        insts0(&f).len(),
        4,
        "a call between loads must block load elimination"
    );
}

/// A load in a block with a single predecessor reuses a load the predecessor left available
/// (extended-basic-block forwarding) — e.g. binary search's `a[mid]` reloaded in the `else`.
#[test]
fn single_pred_load_forwarded() {
    // b0: v2 = load v0; condbr(v2) -> b1, b2
    // b1: ret v2
    // b2 (sole pred b0): v3 = load v0; v4 = v2 + v3; ret v4   → v3 reuses v2
    let f = run(func(
        5,
        vec![
            block(
                0,
                vec![load(2, Val::Reg(0))],
                Term::CondBr {
                    cond: Cond::NonZero {
                        val: Val::Reg(2),
                        ty: Ty::I64,
                    },
                    t: 1,
                    f: 2,
                },
            ),
            block(1, vec![], Term::Ret(Some(Val::Reg(2)))),
            block(
                2,
                vec![load(3, Val::Reg(0)), add(4, Val::Reg(2), Val::Reg(3))],
                Term::Ret(Some(Val::Reg(4))),
            ),
        ],
    ));
    assert_eq!(
        f.blocks[2].insts.len(),
        1,
        "the forwarded load should be removed"
    );
    assert_eq!(
        bin_operands_at(&f, 2, 0),
        (Val::Reg(2), Val::Reg(2)),
        "reload redirected to the predecessor's load"
    );
}

/// A load in a *merge* block (multiple predecessors) is NOT forwarded from a dominating load —
/// one incoming path could have clobbered memory (here the `b1` store).
#[test]
fn merge_block_load_not_forwarded() {
    // b0: v2 = load v0; condbr(v2) -> b1, b2
    // b1: store v0 <- 0; br b3
    // b2: br b3
    // b3 (preds b1,b2): v3 = load v0; ret v3   (must survive — the b1 path clobbered)
    let f = run(func(
        4,
        vec![
            block(
                0,
                vec![load(2, Val::Reg(0))],
                Term::CondBr {
                    cond: Cond::NonZero {
                        val: Val::Reg(2),
                        ty: Ty::I64,
                    },
                    t: 1,
                    f: 2,
                },
            ),
            block(
                1,
                vec![Inst::Store {
                    ty: Ty::I64,
                    addr: Val::Reg(0),
                    val: Val::ImmInt(0),
                }],
                Term::Br(3),
            ),
            block(2, vec![], Term::Br(3)),
            block(3, vec![load(3, Val::Reg(0))], Term::Ret(Some(Val::Reg(3)))),
        ],
    ));
    assert_eq!(
        f.blocks[3].insts.len(),
        1,
        "a merge-block load must not be forwarded"
    );
}
