//! Unit tests for the preprocessor: object/function macros, `#undef`, nested
//! expansion, and conditional compilation (`#ifdef`/`#ifndef`/`#if`/`#elif`/`#else`).
//! Driven entirely in-memory — `#include` file resolution is covered by the
//! `tests/` integration crates, not here.

use super::*;

/// Run `src` through a bare preprocessor and collect the emitted token kinds
/// (trailing `Eof` excluded). Panics on a preprocessor error.
fn pp(src: &str) -> Vec<TokenKind> {
    let mut p = Preprocessor::new(Lexer::new(src));
    let mut out = Vec::new();
    loop {
        let t = p.next_token().expect("preproc");
        if t.kind == TokenKind::Eof {
            return out;
        }
        out.push(t.kind);
    }
}

/// Like [`pp`], but surfaces a preprocessor error as `Err` instead of panicking.
fn pp_result(src: &str) -> Result<Vec<TokenKind>, ()> {
    let mut p = Preprocessor::new(Lexer::new(src));
    let mut out = Vec::new();
    loop {
        match p.next_token() {
            Ok(t) if t.kind == TokenKind::Eof => return Ok(out),
            Ok(t) => out.push(t.kind),
            Err(_) => return Err(()),
        }
    }
}

#[test]
fn object_macro_expands() {
    assert_eq!(pp("#define N 5\nN"), vec![TokenKind::Int(5)]);
}

#[test]
fn object_macro_multiple_tokens() {
    assert_eq!(
        pp("#define PT 3 + 4\nPT"),
        vec![TokenKind::Int(3), TokenKind::Plus, TokenKind::Int(4)],
    );
}

#[test]
fn function_macro_substitutes_args() {
    assert_eq!(
        pp("#define ADD(a, b) a + b\nADD(1, 2)"),
        vec![TokenKind::Int(1), TokenKind::Plus, TokenKind::Int(2)],
    );
}

#[test]
fn undef_disables_macro() {
    assert_eq!(
        pp("#define N 5\n#undef N\nN"),
        vec![TokenKind::Ident("N".into())],
    );
}

#[test]
fn nested_macro_expansion() {
    // A expands to B, which expands to 42.
    assert_eq!(pp("#define A B\n#define B 42\nA"), vec![TokenKind::Int(42)]);
}

#[test]
fn ifdef_else_endif() {
    assert_eq!(
        pp("#define FOO\n#ifdef FOO\n1\n#else\n2\n#endif"),
        vec![TokenKind::Int(1)],
    );
    assert_eq!(
        pp("#ifdef FOO\n1\n#else\n2\n#endif"),
        vec![TokenKind::Int(2)],
    );
}

#[test]
fn ifndef_includes_when_absent() {
    assert_eq!(pp("#ifndef BAR\n7\n#endif"), vec![TokenKind::Int(7)]);
    assert_eq!(pp("#define BAR\n#ifndef BAR\n7\n#endif"), vec![]);
}

#[test]
fn if_defined_with_boolean_ops() {
    assert_eq!(
        pp("#define A\n#if defined(A) || defined(B)\n9\n#endif"),
        vec![TokenKind::Int(9)],
    );
    assert_eq!(pp("#if !defined(X)\n3\n#endif"), vec![TokenKind::Int(3)]);
    assert_eq!(
        pp("#define A\n#if defined(A) && defined(B)\n9\n#endif"),
        vec![]
    );
}

#[test]
fn elif_chain_selects_first_true() {
    let when_y = "#define Y\n#if defined(X)\n1\n#elif defined(Y)\n2\n#else\n3\n#endif";
    assert_eq!(pp(when_y), vec![TokenKind::Int(2)]);
    let when_none = "#if defined(X)\n1\n#elif defined(Y)\n2\n#else\n3\n#endif";
    assert_eq!(pp(when_none), vec![TokenKind::Int(3)]);
}

#[test]
fn integer_condition_truthiness() {
    assert_eq!(pp("#if 1\n8\n#endif"), vec![TokenKind::Int(8)]);
    assert_eq!(pp("#if 0\n8\n#endif"), vec![]);
}

#[test]
fn unterminated_conditional_errors() {
    assert!(pp_result("#ifdef FOO\n1\n").is_err());
}

// ---- deferred implementation: a `.hh` header auto-pairs its `.hc` impl, which is
//      streamed after the primary source (file resolution, so temp-dir based) ----

/// Write `files` into a fresh temp dir, then preprocess `src` with that dir on the
/// angle-include search path. Returns the emitted **identifier** names in order — the
/// marker tokens the test files carry — so a test can see header-inline vs impl-at-end.
fn pp_with_lib(src: &str, files: &[(&str, &str)]) -> Vec<String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("hcc-pp-defer-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (name, content) in files {
        std::fs::write(dir.join(name), content).unwrap();
    }
    let mut p =
        Preprocessor::with_base_dir_and_search(Lexer::new(src), dir.clone(), vec![dir.clone()]);
    let mut out = Vec::new();
    loop {
        match p.next_token().expect("preproc") {
            t if t.kind == TokenKind::Eof => break,
            t => {
                if let TokenKind::Ident(s) = t.kind {
                    out.push(s);
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    out
}

#[test]
fn deferred_impl_streams_after_main() {
    // Including <m.hh> expands its declarations inline, but its paired m.hc body is
    // held back until the primary source is exhausted: Dh, then Mn, then Bc.
    let out = pp_with_lib("#include <m.hh>\nMn", &[("m.hh", "Dh"), ("m.hc", "Bc")]);
    assert_eq!(out, vec!["Dh", "Mn", "Bc"]);
}

#[test]
fn header_only_module_pulls_no_impl() {
    // No sibling .hc → nothing deferred, no error (a header-only module like <limits.hh>).
    let out = pp_with_lib("#include <h.hh>\nMn", &[("h.hh", "Dh")]);
    assert_eq!(out, vec!["Dh", "Mn"]);
}

#[test]
fn deferred_impl_streamed_once_for_repeated_include() {
    // The guarded header is expanded once; the impl is deduped independently and also
    // streamed once, even though <m.hh> is included twice.
    let hdr = "#ifndef _M_HH\n#define _M_HH\nDh\n#endif";
    let out = pp_with_lib(
        "#include <m.hh>\n#include <m.hh>\nMn",
        &[("m.hh", hdr), ("m.hc", "Bc")],
    );
    assert_eq!(out, vec!["Dh", "Mn", "Bc"]);
}

#[test]
fn impl_directive_queues_named_implementation() {
    // A header whose impl isn't the filename pair names it with `#impl`.
    let out = pp_with_lib(
        "#include <api.hh>\nMn",
        &[("api.hh", "#impl <api_impl.hc>\nDh"), ("api_impl.hc", "Bc")],
    );
    assert_eq!(out, vec!["Dh", "Mn", "Bc"]);
}

#[test]
fn deferred_impls_drain_transitively() {
    // a.hc (deferred) itself includes <b.hh>, which expands inline and queues b.hc — so
    // the end-batch grows and drains to a fixpoint: Da, Mn, then a.hc → (Db inline, Ba),
    // then b.hc → Bb.
    let out = pp_with_lib(
        "#include <a.hh>\nMn",
        &[
            ("a.hh", "Da"),
            ("a.hc", "#include <b.hh>\nBa"),
            ("b.hh", "Db"),
            ("b.hc", "Bb"),
        ],
    );
    assert_eq!(out, vec!["Da", "Mn", "Db", "Ba", "Bb"]);
}
