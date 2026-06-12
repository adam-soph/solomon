//! Unit tests for AST→SSA lowering. These assert IR-level properties of
//! `lower(program, layouts)` on tiny functions: the GCC-style register/memory split
//! (a non-address-taken scalar is a vreg/phi; an address-taken or aggregate local
//! gets a frame slot reached by `SlotAddr`/`Load`/`Store`), on-the-fly phi insertion
//! for loop-carried values, and control-flow lowering (`if`→`CondBr`, `switch`→a
//! `Switch` terminator with const-folded case ranges).

use super::*;

/// Parse → sema (annotates `e.ty()`, which lowering reads) → layout → lower.
fn lower_src(src: &str) -> crate::ir::Program {
    let prog = crate::parser::parse(src).expect("parse");
    let serrs = crate::sema::check_program(&prog);
    assert!(serrs.is_empty(), "sema: {serrs:?}");
    let (layouts, lerrs) = crate::layout::compute(&prog);
    assert!(lerrs.is_empty(), "layout: {lerrs:?}");
    lower(&prog, &layouts).expect("lower")
}

/// The lowered function named `name`.
fn func<'a>(p: &'a crate::ir::Program, name: &str) -> &'a Func {
    p.funcs
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("no lowered function `{name}` (have {:?})", names(p)))
}

fn names(p: &crate::ir::Program) -> Vec<&str> {
    p.funcs.iter().map(|f| f.name.as_str()).collect()
}

/// Whether any instruction in `f` satisfies `pred`.
fn has(f: &Func, pred: impl Fn(&Inst) -> bool) -> bool {
    f.blocks.iter().flat_map(|b| &b.insts).any(|i| pred(i))
}

fn has_local_slot(f: &Func) -> bool {
    f.slots.iter().any(|s| s.kind == SlotKind::Local)
}

#[test]
fn plain_scalar_local_stays_in_ssa() {
    let p = lower_src("I64 Add1(I64 n) { I64 x = n + 1; return x; }");
    let f = func(&p, "Add1");
    // `x` is never address-taken ⇒ no frame slot and no memory traffic for it.
    assert!(!has_local_slot(f), "unexpected Local slot: {:?}", f.slots);
    assert!(
        !has(f, |i| matches!(
            i,
            Inst::Load { .. } | Inst::Store { .. } | Inst::SlotAddr { .. }
        )),
        "an SSA scalar should not touch memory",
    );
    // The `n + 1` is real arithmetic (n is not constant).
    assert!(has(f, |i| matches!(i, Inst::Bin { .. })));
}

#[test]
fn address_taken_scalar_gets_a_slot() {
    let p = lower_src("I64 Deref() { I64 x = 5; I64 *q = &x; return *q; }");
    let f = func(&p, "Deref");
    // Taking `&x` forces `x` into an addressable frame slot.
    assert!(has_local_slot(f), "expected a Local slot: {:?}", f.slots);
    assert!(
        has(f, |i| matches!(i, Inst::SlotAddr { .. })),
        "expected SlotAddr"
    );
    assert!(
        has(f, |i| matches!(i, Inst::Store { .. })),
        "expected Store"
    );
    assert!(has(f, |i| matches!(i, Inst::Load { .. })), "expected Load");
}

#[test]
fn aggregate_local_gets_a_slot() {
    let p = lower_src("class P { I64 x; I64 y; } P Make() { P p; p.x = 1; p.y = 2; return p; }");
    let f = func(&p, "Make");
    // An aggregate is always memory-resident.
    assert!(
        has_local_slot(f),
        "aggregate local should get a slot: {:?}",
        f.slots
    );
    assert!(
        has(f, |i| matches!(i, Inst::Store { .. })),
        "field writes go through the slot"
    );
}

#[test]
fn loop_carried_scalars_become_phis() {
    let p = lower_src(
        "I64 Sum(I64 n) { I64 s = 0; I64 i; for (i = 0; i < n; i++) s = s + i; return s; }",
    );
    let f = func(&p, "Sum");
    // `s` and `i` are loop-carried but not address-taken: on-the-fly SSA puts them in
    // phis at the loop header, never in a slot.
    assert!(
        f.blocks.iter().any(|b| !b.phis.is_empty()),
        "expected phi nodes for loop-carried values",
    );
    assert!(
        !has_local_slot(f),
        "loop-carried scalars should not be spilled: {:?}",
        f.slots
    );
}

#[test]
fn if_else_lowers_to_a_conditional_branch() {
    let p = lower_src("I64 Sign(I64 n) { if (n < 0) return -1; return 1; }");
    let f = func(&p, "Sign");
    assert!(
        f.blocks
            .iter()
            .any(|b| matches!(b.term, Term::CondBr { .. })),
        "an `if` should lower to a CondBr terminator",
    );
}

#[test]
fn switch_lowers_to_folded_case_ranges() {
    let p = lower_src(
        "I64 Pick(I64 x) { switch (x) { case 1: { return 10; } case 2: { return 20; } } return 0; }",
    );
    let f = func(&p, "Pick");
    // A `switch` lowers to a Switch terminator whose case labels are const-folded into
    // inclusive `(lo, hi)` ranges (this is the const-folding lowering actually does).
    let cases = f
        .blocks
        .iter()
        .find_map(|b| match &b.term {
            Term::Switch { cases, .. } => Some(cases.clone()),
            _ => None,
        })
        .expect("switch should lower to a Switch terminator");
    let los: Vec<i64> = cases.iter().map(|(lo, _, _)| *lo).collect();
    assert!(
        los.contains(&1) && los.contains(&2),
        "case labels 1 and 2 should be folded into ranges, got {cases:?}",
    );
}

#[test]
fn every_defined_function_is_lowered() {
    // Lowering produces an Func per defined function plus the synthetic `@entry`.
    let p = lower_src("I64 A() { return 1; } I64 B() { return 2; }");
    assert!(names(&p).contains(&"A"));
    assert!(names(&p).contains(&"B"));
    assert!(names(&p).contains(&"@entry"));
}
