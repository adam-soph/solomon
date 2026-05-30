//! Output-level tests for the larger sample programs: they don't just run
//! cleanly, they compute the right answers.

use solomon::backend::interp::run_to_string;
use solomon::parser::parse;

/// Parse and run a sample, returning everything it printed.
fn run(name: &str, src: &str) -> String {
    let program = parse(src).unwrap_or_else(|e| panic!("{name}: parse failed: {e}"));
    run_to_string(&program).unwrap_or_else(|e| panic!("{name}: {e}"))
}

#[test]
fn linklist_sorts_and_computes() {
    let out = run("linklist.hc", include_str!("data/linklist.hc"));
    assert_eq!(out, "sorted: 1 2 3 5 7 8 9 \nlength=7 gcd(48,36)=12\n");
}

#[test]
fn vm_evaluates_program() {
    // -((2 + 3) * 4 - 5) = -15
    let out = run("vm.hc", include_str!("data/vm.hc"));
    assert_eq!(out, "vm result = -15\n");
}

#[test]
fn mathlib_macros_and_algorithms() {
    let out = run("mathlib.hc", include_str!("data/mathlib.hc"));
    assert_eq!(
        out,
        "abs=7 max=12 min=-7 clamp=10\n\
         sq6=36 ipow=1024 isqrt=12 popcount=8\n\
         fancy enabled\n\
         release build\n"
    );
}

#[test]
fn shapes_dispatch_and_inheritance() {
    let out = run("shapes.hc", include_str!("data/shapes.hc"));
    // rect = 3*4 = 12, tri = 0.5*6*5 = 15, rect not bigger than tri (12 < 15).
    assert!(out.contains("rect area = 12\n"), "got: {out}");
    assert!(out.contains("tri area = 15\n"), "got: {out}");
    assert!(out.contains("rect bigger than tri? 0\n"), "got: {out}");
}

#[test]
fn matrix_multiply_trace() {
    let out = run("matrix.hc", include_str!("data/matrix.hc"));
    // (2I)(ones) is all 2s, so trace = 6 and each entry is 2.
    assert!(out.contains("trace = 6\n"), "got: {out}");
    assert!(out.contains("c[0][0]=2 c[2][1]=2\n"), "got: {out}");
}
