//! Directory-scoped visibility via the `public` keyword. A top-level symbol (function,
//! `class`/`union`, or global) is visible to every file in the **same directory**;
//! crossing a directory boundary needs `public`. The rule is enforced in sema and
//! applies to all code — the standard library and user programs alike. (This replaced
//! the older `_`-prefix directory privacy.)

use solomon::parser::{parse, parse_in_dir};
use solomon::sema::check_program;

fn errors(program: &solomon::Program) -> Vec<String> {
    check_program(program)
        .into_iter()
        .map(|e| e.message)
        .collect()
}

/// Parse a main file that `#include`s a `header` sitting **in the same directory**, then
/// runs `main_body`. Same-directory references see non-`public` symbols.
fn same_dir(tag: &str, header: &str, main_body: &str) -> Vec<String> {
    in_dirs(tag, header, "h.hc", "h.hc", main_body)
}

/// Parse a main file that `#include`s a `header` in a **subdirectory** (a different
/// directory), then runs `main_body`. Cross-directory references need `public`.
fn cross_dir(tag: &str, header: &str, main_body: &str) -> Vec<String> {
    in_dirs(tag, header, "sub/h.hc", "sub/h.hc", main_body)
}

fn in_dirs(
    tag: &str,
    header: &str,
    header_path: &str,
    include: &str,
    main_body: &str,
) -> Vec<String> {
    let dir = std::env::temp_dir().join(format!("solomon-vis-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let hpath = dir.join(header_path);
    std::fs::create_dir_all(hpath.parent().unwrap()).unwrap();
    std::fs::write(&hpath, header).unwrap();
    let src = format!("#include \"{include}\"\nU0 Main() {{ {main_body} }}\nMain;");
    let program = parse_in_dir(&src, &dir).unwrap_or_else(|e| panic!("parse failed: {e}"));
    let errs = errors(&program);
    let _ = std::fs::remove_dir_all(&dir);
    errs
}

fn is_private_error(errs: &[String], name: &str) -> bool {
    errs.iter()
        .any(|m| m.contains(name) && m.contains("not `public`"))
}

// ---- functions ----------------------------------------------------------------

#[test]
fn public_function_is_callable_across_directories() {
    let errs = cross_dir("fn-ok", "public I64 Helper() { return 42; }", "Helper();");
    assert!(errs.is_empty(), "unexpected errors: {errs:?}");
}

#[test]
fn non_public_function_is_visible_within_the_same_directory() {
    let errs = same_dir("fn-same", "I64 Helper() { return 42; }", "Helper();");
    assert!(
        errs.is_empty(),
        "same-directory use should be allowed: {errs:?}"
    );
}

#[test]
fn non_public_function_is_private_across_directories() {
    let errs = cross_dir("fn-bad", "I64 Helper() { return 42; }", "Helper();");
    assert!(
        is_private_error(&errs, "Helper"),
        "expected a cross-directory visibility error for `Helper`, got: {errs:?}"
    );
}

// ---- types --------------------------------------------------------------------

#[test]
fn public_type_is_usable_across_directories() {
    let errs = cross_dir(
        "ty-ok",
        "public class Pt { I64 x; I64 y; }",
        "Pt p; p.x = 1;",
    );
    assert!(errs.is_empty(), "unexpected errors: {errs:?}");
}

#[test]
fn non_public_type_is_visible_within_the_same_directory() {
    let errs = same_dir("ty-same", "class Pt { I64 x; I64 y; }", "Pt p; p.x = 1;");
    assert!(
        errs.is_empty(),
        "same-directory use should be allowed: {errs:?}"
    );
}

#[test]
fn non_public_type_is_private_across_directories() {
    let errs = cross_dir("ty-bad", "class Pt { I64 x; I64 y; }", "Pt p; p.x = 1;");
    assert!(
        is_private_error(&errs, "Pt"),
        "expected a cross-directory visibility error for `Pt`, got: {errs:?}"
    );
}

// ---- globals ------------------------------------------------------------------

#[test]
fn public_global_is_visible_across_directories() {
    let errs = cross_dir("g-ok", "public I64 G = 7;", "I64 x = G;");
    assert!(errs.is_empty(), "unexpected errors: {errs:?}");
}

#[test]
fn non_public_global_is_visible_within_the_same_directory() {
    let errs = same_dir("g-same", "I64 G = 7;", "I64 x = G;");
    assert!(
        errs.is_empty(),
        "same-directory use should be allowed: {errs:?}"
    );
}

#[test]
fn non_public_global_is_private_across_directories() {
    let errs = cross_dir("g-bad", "I64 G = 7;", "I64 x = G;");
    assert!(
        is_private_error(&errs, "G"),
        "expected a cross-directory visibility error for `G`, got: {errs:?}"
    );
}

// ---- same-file & misuse -------------------------------------------------------

#[test]
fn non_public_symbol_is_visible_within_its_own_file() {
    // No `public` needed for a same-file reference.
    let src = "I64 Helper() { return 9; }\nU0 Main() { Helper(); }\nMain;";
    let program = parse(src).unwrap_or_else(|e| panic!("parse failed: {e}"));
    assert!(
        errors(&program).is_empty(),
        "same-file use should be allowed: {:?}",
        errors(&program)
    );
}

#[test]
fn public_on_a_local_is_an_error() {
    let src = "U0 Main() { public I64 x; }\nMain;";
    let program = parse(src).unwrap_or_else(|e| panic!("parse failed: {e}"));
    let errs = errors(&program);
    assert!(
        errs.iter()
            .any(|m| m.contains("`public`") && m.contains("top-level")),
        "expected a top-level-only error, got: {errs:?}"
    );
}

// ---- standard library ---------------------------------------------------------

#[test]
fn public_stdlib_symbol_is_callable() {
    // The stdlib's public API is callable from user code across the file boundary.
    let src = "#include <cstr.hc>\nU0 Main() { U8 *s = \"hi\"; StrLen(s); }\nMain;";
    let program = parse(src).unwrap_or_else(|e| panic!("parse failed: {e}"));
    assert!(
        errors(&program).is_empty(),
        "calling a public stdlib symbol should be fine: {:?}",
        errors(&program)
    );
}

#[test]
fn private_stdlib_helper_is_rejected_from_user_code() {
    // `PfPut` is a non-`public` helper in the stdlib's `printf.hc`. User code lives in a
    // different directory than the embedded `<stdlib>`, so the cross-directory rule
    // rejects the call.
    let src = "#include <printf.hc>\nU0 Main() { Pf p; PfPut(&p, \"x\", 1); }\nMain;";
    let program = parse(src).unwrap_or_else(|e| panic!("parse failed: {e}"));
    let errs = errors(&program);
    assert!(
        is_private_error(&errs, "PfPut"),
        "expected a visibility error for `PfPut`, got: {errs:?}"
    );
}

#[test]
fn public_function_may_not_return_a_non_public_type() {
    // A `public` function would expose a non-`public` type a cross-file caller can't
    // name. Flagged for the underlying named type, peeling pointers.
    for ret in ["Secret", "Secret *"] {
        let src = format!("class Secret {{ I64 x; }}\npublic {ret} Make() {{ return NULL; }}");
        let program = parse(&src).unwrap_or_else(|e| panic!("parse failed: {e}"));
        let errs = errors(&program);
        assert!(
            errs.iter()
                .any(|m| m.contains("Make") && m.contains("Secret") && m.contains("non-`public`")),
            "expected a public-return-type error for `{ret}`, got: {errs:?}"
        );
    }
    // A `public` return type is fine; a non-`public` function returning a private type
    // is also fine (it's not part of the cross-file API surface).
    let ok = "public class Ok { I64 x; }\npublic Ok MkOk() { Ok s; s.x = 1; return s; }\n\
              class Hidden { I64 y; }\nHidden MkHidden() { Hidden h; return h; }";
    let program = parse(ok).unwrap_or_else(|e| panic!("parse failed: {e}"));
    assert!(
        errors(&program).is_empty(),
        "public->public and private->private should be fine: {:?}",
        errors(&program)
    );
}
