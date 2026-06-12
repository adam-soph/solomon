//! Tests for parallel-copy sequentialization (`sequence_copies`, the T4c copy-coalescing
//! core). The invariant: executing the emitted `Mov`s in order must realize the *parallel*
//! copy — every destination ends holding the value its source held *before* any move ran.

use std::collections::HashMap;

use crate::backend::sequence_copies;
use crate::ir::{Inst, Ty, Val, Vreg};

fn eval(reg: &HashMap<Vreg, i64>, v: Val) -> i64 {
    match v {
        Val::Reg(r) => reg[&r],
        Val::ImmInt(i) => i,
        Val::ImmF64(b) => b as i64,
    }
}

/// Run `sequence_copies`, execute the result over a register file seeded so each vreg holds a
/// unique sentinel (its own id), and assert every destination ends with its source's *initial*
/// value — i.e. the simultaneous-copy semantics were preserved.
fn check(copies: &[(Vreg, Ty, Val)]) {
    let mut next = 1000u32; // fresh temps start above every vreg used in these cases
    let movs = sequence_copies(copies, &mut next);

    let mut reg: HashMap<Vreg, i64> = HashMap::new();
    for (d, _, s) in copies {
        reg.entry(*d).or_insert(*d as i64);
        if let Val::Reg(r) = s {
            reg.entry(*r).or_insert(*r as i64);
        }
    }
    let expected: Vec<(Vreg, i64)> = copies
        .iter()
        .map(|(d, _, s)| (*d, eval(&reg, *s)))
        .collect();

    for m in &movs {
        let Inst::Mov { dst, src, .. } = m else {
            panic!("sequence_copies emitted a non-Mov: {m:?}");
        };
        let v = eval(&reg, *src);
        reg.insert(*dst, v);
    }

    for (d, want) in expected {
        assert_eq!(
            reg[&d], want,
            "dst {d} should hold the initial value of its source; copies={copies:?}"
        );
    }
}

#[test]
fn parallel_copy_semantics_preserved() {
    let i = Ty::I64;
    // Independent copies.
    check(&[(1, i, Val::Reg(2)), (3, i, Val::Reg(4))]);
    // Needs reordering (acyclic): `3 := 1` must precede `1 := 2`.
    check(&[(1, i, Val::Reg(2)), (3, i, Val::Reg(1))]);
    // A swap (2-cycle) — needs a temporary.
    check(&[(1, i, Val::Reg(2)), (2, i, Val::Reg(1))]);
    // A 3-cycle.
    check(&[
        (1, i, Val::Reg(2)),
        (2, i, Val::Reg(3)),
        (3, i, Val::Reg(1)),
    ]);
    // Immediate source mixed with a dependent reg source.
    check(&[(1, i, Val::ImmInt(42)), (2, i, Val::Reg(1))]);
    // A no-op self-copy alongside a real one.
    check(&[(1, i, Val::Reg(1)), (2, i, Val::Reg(3))]);
    // A longer dependency chain.
    check(&[
        (1, i, Val::Reg(2)),
        (2, i, Val::Reg(3)),
        (4, i, Val::Reg(1)),
    ]);
    // A cycle plus an independent tail (mixed cyclic/acyclic in one set).
    check(&[
        (1, i, Val::Reg(2)),
        (2, i, Val::Reg(1)),
        (5, i, Val::Reg(6)),
    ]);
}
