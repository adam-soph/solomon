//! Tests for [`allocate`]'s promotion tiers — in particular the single-use (refs == 1)
//! caller-only tier added on top of the original refs ≥ 2 tier. Fixtures are hand-built
//! `Func`s (already out of SSA, as `allocate` expects); the assertions read the resulting
//! [`Location`] per vreg.

use super::*;

/// A `RegSet` with one int register in each of the caller/callee pools (and none for floats),
/// so a test can observe exactly which pool a vreg drew from. The caller register is 100, the
/// callee 200.
fn one_each() -> RegSet {
    RegSet {
        callee_int: vec![200],
        caller_int: vec![100],
        callee_flt: vec![],
        caller_flt: vec![],
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

fn func(n_vregs: u32, params: Vec<Param>, blocks: Vec<Block>) -> Func {
    Func {
        name: "@test".into(),
        ret: Ret::Scalar(Ty::I64),
        params,
        varargs: false,
        slots: vec![],
        blocks,
        entry: 0,
        n_vregs,
    }
}

fn param(vreg: Vreg) -> Param {
    Param {
        ty: ArgTy::Int(Ty::I64),
        vreg,
        name: None,
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

fn call(dst: Option<Vreg>, args: Vec<Val>) -> Inst {
    Inst::Call {
        dst,
        ret: if dst.is_some() {
            Ret::Scalar(Ty::I64)
        } else {
            Ret::Void
        },
        callee: Callee::Direct("f".into()),
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

fn reg_num(a: &Allocation, v: Vreg) -> Option<u32> {
    match a.loc[v as usize] {
        Location::Reg(r) => Some(r.num),
        Location::Spill => None,
    }
}

/// A single-use, non-call-crossing temp draws a *caller-saved* register (no prologue save).
#[test]
fn single_use_temp_takes_caller_reg() {
    // v1 = 2 + 1; ret v1   — v1 is the sole candidate (no param competing for the pool),
    // referenced once, never crosses a call.
    let f = func(
        2,
        vec![],
        vec![block(
            0,
            vec![add(1, Val::ImmInt(2), Val::ImmInt(1))],
            Term::Ret(Some(Val::Reg(1))),
        )],
    );
    let a = allocate(&f, &one_each(), false);
    assert_eq!(
        reg_num(&a, 1),
        Some(100),
        "single-use temp should be in caller reg 100"
    );
    assert!(
        a.saved.is_empty(),
        "a caller-saved promotion needs no prologue save"
    );
}

/// A single-use temp does **not** fall back to a callee-saved register when the caller pool is
/// empty (the prologue save would outweigh the one avoided spill) — it stays spilled.
#[test]
fn single_use_temp_no_callee_fallback() {
    let regs = RegSet {
        callee_int: vec![200],
        caller_int: vec![], // no caller registers
        callee_flt: vec![],
        caller_flt: vec![],
    };
    let f = func(
        2,
        vec![param(0)],
        vec![block(
            0,
            vec![add(1, Val::Reg(0), Val::ImmInt(1))],
            Term::Ret(Some(Val::Reg(1))),
        )],
    );
    let a = allocate(&f, &regs, false);
    assert_eq!(
        reg_num(&a, 1),
        None,
        "single-use temp must not take a callee reg"
    );
    assert!(a.saved.is_empty());
}

/// A single-use temp whose live range crosses a call would need a callee-saved register (a
/// prologue save) — not worth it for one use, so it stays spilled.
#[test]
fn single_use_crossing_temp_spills() {
    // v1 = v0 + 1; call f(); ret v1   — v1 is live across the call.
    let f = func(
        2,
        vec![param(0)],
        vec![block(
            0,
            vec![add(1, Val::Reg(0), Val::ImmInt(1)), call(None, vec![])],
            Term::Ret(Some(Val::Reg(1))),
        )],
    );
    let a = allocate(&f, &one_each(), false);
    assert_eq!(
        reg_num(&a, 1),
        None,
        "single-use crossing temp should stay spilled"
    );
}

/// A multi-use (refs ≥ 2) vreg still promotes — the original tier is unchanged. Here v1 is
/// used twice and never crosses a call, so it takes the caller register.
#[test]
fn multi_use_still_promotes() {
    // v1 = v0 + 1; v2 = v1 + v1; ret v2
    let f = func(
        3,
        vec![param(0)],
        vec![block(
            0,
            vec![
                add(1, Val::Reg(0), Val::ImmInt(1)),
                add(2, Val::Reg(1), Val::Reg(1)),
            ],
            Term::Ret(Some(Val::Reg(2))),
        )],
    );
    let a = allocate(&f, &one_each(), false);
    assert_eq!(
        reg_num(&a, 1),
        Some(100),
        "multi-use v1 should promote to caller reg 100"
    );
}

/// A single-use temp never *evicts* an active holder: when the only register is already held by
/// a hotter (multi-use) vreg, the single-use temp stays spilled rather than displacing it.
#[test]
fn single_use_temp_never_evicts() {
    // v1 = v0 + 1 (used twice, spans the whole block → wants the callee reg across the call);
    // single-use v2 wants a register while v1 is active, but the caller pool is empty and it
    // must not evict v1.
    let regs = RegSet {
        callee_int: vec![200],
        caller_int: vec![], // force contention on the single callee reg
        callee_flt: vec![],
        caller_flt: vec![],
    };
    // v1 = v0 + 1; call f(v1); v2 = v1 + 1; ret v1   (v1: 3 refs, crosses the call → callee reg)
    let f = func(
        3,
        vec![param(0)],
        vec![block(
            0,
            vec![
                add(1, Val::Reg(0), Val::ImmInt(1)),
                call(None, vec![Val::Reg(1)]),
                add(2, Val::Reg(1), Val::ImmInt(1)),
            ],
            Term::Ret(Some(Val::Reg(1))),
        )],
    );
    let a = allocate(&f, &regs, false);
    assert_eq!(
        reg_num(&a, 1),
        Some(200),
        "hot multi-use v1 holds the callee reg"
    );
    assert_eq!(reg_num(&a, 2), None, "single-use v2 must not evict v1");
}

/// `program_has_exceptions` forces everything spilled, in every function (a `throw`'s longjmp
/// would skip the epilogue that restores callee-saved registers).
#[test]
fn exceptions_spill_everything() {
    let f = func(
        3,
        vec![param(0)],
        vec![block(
            0,
            vec![
                add(1, Val::Reg(0), Val::ImmInt(1)),
                add(2, Val::Reg(1), Val::Reg(1)),
            ],
            Term::Ret(Some(Val::Reg(2))),
        )],
    );
    let a = allocate(&f, &one_each(), true);
    assert_eq!(reg_num(&a, 1), None);
    assert_eq!(reg_num(&a, 2), None);
}
