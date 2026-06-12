//! Tests for the function inliner: a small non-recursive callee is spliced in (the call
//! disappears, the result is forwarded), and the excluded shapes (recursive, aggregate-return,
//! exception-bearing) are left alone. Every rewrite is checked with `verify()`.

use super::*;
use crate::layout::Layouts;

fn prog(funcs: Vec<Func>) -> Program {
    Program {
        funcs,
        globals: vec![],
        strings: vec![],
        layouts: Layouts::default(),
    }
}

fn param(v: Vreg) -> Param {
    Param {
        ty: ArgTy::Int(Ty::I64),
        vreg: v,
        name: None,
    }
}

fn call(dst: Option<Vreg>, callee: &str, args: Vec<Val>) -> Inst {
    Inst::Call {
        dst,
        ret: if dst.is_some() {
            Ret::Scalar(Ty::I64)
        } else {
            Ret::Void
        },
        callee: Callee::Direct(callee.into()),
        args: args
            .into_iter()
            .map(|val| ArgVal {
                ty: ArgTy::Int(Ty::I64),
                val,
            })
            .collect(),
        sret: None,
        varargs: VarargInfo::default(),
    }
}

fn run(funcs: Vec<Func>) -> Program {
    let out = super::run(prog(funcs));
    assert!(
        crate::ir::verify(&out).is_empty(),
        "inliner produced invalid IR: {:?}",
        crate::ir::verify(&out)
    );
    out
}

fn caller(out: &Program) -> &Func {
    out.funcs.iter().find(|f| f.name == "@t").unwrap()
}

fn has_call_to(f: &Func, name: &str) -> bool {
    f.blocks
        .iter()
        .flat_map(|b| &b.insts)
        .any(|i| matches!(i, Inst::Call { callee: Callee::Direct(n), .. } if n == name))
}

/// A straight-line single-`Ret` callee is inlined: the call disappears and the body is spliced.
#[test]
fn single_block_callee_inlined() {
    // add1(a) { return a + 1; }
    let add1 = Func {
        name: "add1".into(),
        ret: Ret::Scalar(Ty::I64),
        params: vec![param(0)],
        varargs: false,
        slots: vec![],
        blocks: vec![Block {
            id: 0,
            phis: vec![],
            insts: vec![Inst::Bin {
                dst: 1,
                op: BinOp::Add,
                ty: Ty::I64,
                signed: true,
                lhs: Val::Reg(0),
                rhs: Val::ImmInt(1),
            }],
            term: Term::Ret(Some(Val::Reg(1))),
        }],
        entry: 0,
        n_vregs: 2,
    };
    // @t() { return add1(5); }
    let t = Func {
        name: "@t".into(),
        ret: Ret::Scalar(Ty::I64),
        params: vec![],
        varargs: false,
        slots: vec![],
        blocks: vec![Block {
            id: 0,
            phis: vec![],
            insts: vec![call(Some(0), "add1", vec![Val::ImmInt(5)])],
            term: Term::Ret(Some(Val::Reg(0))),
        }],
        entry: 0,
        n_vregs: 1,
    };
    let out = run(vec![t, add1]);
    assert!(
        !has_call_to(caller(&out), "add1"),
        "the call should be inlined away"
    );
}

/// A multi-block callee (an internal branch) inlines correctly — block ids, vregs, and the
/// branch condition's operand are all offset (the bug that made gcd_grid loop forever).
#[test]
fn multi_block_callee_inlined() {
    // sel(a) { if (a) return a; else return 0; }  — two return points → a result phi.
    let sel = Func {
        name: "sel".into(),
        ret: Ret::Scalar(Ty::I64),
        params: vec![param(0)],
        varargs: false,
        slots: vec![],
        blocks: vec![
            Block {
                id: 0,
                phis: vec![],
                insts: vec![],
                term: Term::CondBr {
                    cond: Cond::NonZero {
                        val: Val::Reg(0),
                        ty: Ty::I64,
                    },
                    t: 1,
                    f: 2,
                },
            },
            Block {
                id: 1,
                phis: vec![],
                insts: vec![],
                term: Term::Ret(Some(Val::Reg(0))),
            },
            Block {
                id: 2,
                phis: vec![],
                insts: vec![],
                term: Term::Ret(Some(Val::ImmInt(0))),
            },
        ],
        entry: 0,
        n_vregs: 1,
    };
    let t = Func {
        name: "@t".into(),
        ret: Ret::Scalar(Ty::I64),
        params: vec![param(0)],
        varargs: false,
        slots: vec![],
        blocks: vec![Block {
            id: 0,
            phis: vec![],
            insts: vec![call(Some(1), "sel", vec![Val::Reg(0)])],
            term: Term::Ret(Some(Val::Reg(1))),
        }],
        entry: 0,
        n_vregs: 2,
    };
    let out = run(vec![t, sel]);
    let c = caller(&out);
    assert!(!has_call_to(c, "sel"), "the call should be inlined away");
    // The continuation holds a result phi over the two return points.
    let has_result_phi = c
        .blocks
        .iter()
        .any(|b| b.phis.iter().any(|p| p.args.len() == 2));
    assert!(has_result_phi, "two returns should merge via a phi");
}

/// A directly-recursive callee is not inlined (would never terminate).
#[test]
fn recursive_not_inlined() {
    let r = Func {
        name: "@t".into(),
        ret: Ret::Scalar(Ty::I64),
        params: vec![param(0)],
        varargs: false,
        slots: vec![],
        blocks: vec![Block {
            id: 0,
            phis: vec![],
            insts: vec![call(Some(1), "@t", vec![Val::Reg(0)])],
            term: Term::Ret(Some(Val::Reg(1))),
        }],
        entry: 0,
        n_vregs: 2,
    };
    let out = run(vec![r]);
    assert!(
        has_call_to(caller(&out), "@t"),
        "direct recursion must not be inlined"
    );
}

/// An aggregate-returning callee (sret) is not inlined.
#[test]
fn agg_return_not_inlined() {
    let mk = Func {
        name: "mk".into(),
        ret: Ret::Agg { size: 16, align: 8 },
        params: vec![],
        varargs: false,
        slots: vec![],
        blocks: vec![Block {
            id: 0,
            phis: vec![],
            insts: vec![],
            term: Term::Ret(None),
        }],
        entry: 0,
        n_vregs: 0,
    };
    let t = Func {
        name: "@t".into(),
        ret: Ret::Void,
        params: vec![],
        varargs: false,
        slots: vec![SlotInfo {
            size: 16,
            align: 8,
            kind: SlotKind::Sret,
            name: None,
        }],
        blocks: vec![Block {
            id: 0,
            phis: vec![],
            insts: vec![
                Inst::SlotAddr {
                    dst: 0,
                    slot: 0,
                    off: 0,
                },
                Inst::Call {
                    dst: None,
                    ret: Ret::Agg { size: 16, align: 8 },
                    callee: Callee::Direct("mk".into()),
                    args: vec![],
                    sret: Some(Val::Reg(0)),
                    varargs: VarargInfo::default(),
                },
            ],
            term: Term::Ret(None),
        }],
        entry: 0,
        n_vregs: 1,
    };
    let out = run(vec![t, mk]);
    assert!(
        has_call_to(caller(&out), "mk"),
        "an aggregate (sret) return must not be inlined"
    );
}
