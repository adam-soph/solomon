//! End-to-end tests over real HolyC sample files in `tests/data/`.
//!
//! Each sample is run twice: first through the lexer on its own (to confirm it
//! tokenizes cleanly), then through the lexer + parser together (to confirm it
//! produces an AST). The sources are embedded with `include_str!` so the tests
//! do not depend on the working directory.

use solomon::backend::interp::run_to_string;
use solomon::lexer::tokenize;
use solomon::parser::parse;
use solomon::sema::check_program;
use solomon::token::TokenKind;

/// (name, source) for every sample file under tests/data/.
const SAMPLES: &[(&str, &str)] = &[
    ("hello.hc", include_str!("data/hello.hc")),
    ("fib.hc", include_str!("data/fib.hc")),
    ("classes.hc", include_str!("data/classes.hc")),
    ("control.hc", include_str!("data/control.hc")),
    ("preproc.hc", include_str!("data/preproc.hc")),
    ("linklist.hc", include_str!("data/linklist.hc")),
    ("shapes.hc", include_str!("data/shapes.hc")),
    ("vm.hc", include_str!("data/vm.hc")),
    ("mathlib.hc", include_str!("data/mathlib.hc")),
    ("matrix.hc", include_str!("data/matrix.hc")),
];

// ---- the lexer on its own ----

#[test]
fn samples_tokenize_cleanly() {
    for (name, src) in SAMPLES {
        let tokens = tokenize(src).unwrap_or_else(|e| panic!("{name}: lex failed: {e}"));

        // Must be terminated by exactly one Eof, and Eof must not appear early.
        assert_eq!(
            tokens.last().map(|t| &t.kind),
            Some(&TokenKind::Eof),
            "{name}: token stream must end with Eof"
        );
        let eof_count = tokens.iter().filter(|t| t.kind == TokenKind::Eof).count();
        assert_eq!(eof_count, 1, "{name}: exactly one Eof expected");

        // Each sample has real content beyond Eof.
        assert!(
            tokens.len() > 1,
            "{name}: expected a non-empty token stream"
        );

        // Positions advance monotonically (sanity check on span tracking).
        for w in tokens.windows(2) {
            let (a, b) = (&w[0].span, &w[1].span);
            assert!(
                b.start >= a.start,
                "{name}: token spans should be non-decreasing"
            );
        }
    }
}

// ---- the lexer and parser together ----

#[test]
fn samples_parse_cleanly() {
    for (name, src) in SAMPLES {
        let program = parse(src).unwrap_or_else(|e| panic!("{name}: parse failed: {e}"));
        assert!(
            !program.items.is_empty(),
            "{name}: expected at least one top-level item"
        );
    }
}

// ---- the full front end: lexer + parser + semantic analysis ----

#[test]
fn samples_pass_semantic_analysis() {
    for (name, src) in SAMPLES {
        let program = parse(src).unwrap_or_else(|e| panic!("{name}: parse failed: {e}"));
        let errors = check_program(&program);
        assert!(
            errors.is_empty(),
            "{name}: expected no semantic errors, got: {errors:?}"
        );
    }
}

#[test]
fn samples_run_without_error() {
    // Every sample should execute to completion. Most define library functions
    // and produce no output; hello.hc calls Main and prints.
    for (name, src) in SAMPLES {
        let program = parse(src).unwrap_or_else(|e| panic!("{name}: parse failed: {e}"));
        let out = run_to_string(&program).unwrap_or_else(|e| panic!("{name}: runtime error: {e}"));
        if *name == "hello.hc" {
            assert!(
                out.contains("Hello, World!"),
                "hello.hc should greet, got: {out:?}"
            );
        }
    }
}
