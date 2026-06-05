//! Tests for the preprocessor (macro expansion + conditionals) and for type
//! hoisting in the two-pass `parse`.

use solomon::lexer::{Lexer, TokenStream};
use solomon::parser::{parse, parse_in_dir};
use solomon::preproc::Preprocessor;
use solomon::sema::check_program;
use solomon::token::TokenKind;

/// Run `src` through the preprocessor and collect the resulting token kinds
/// (excluding the trailing Eof).
fn pp(src: &str) -> Vec<TokenKind> {
    let mut p = Preprocessor::new(Lexer::new(src));
    let mut out = Vec::new();
    loop {
        let t = p
            .next_token()
            .unwrap_or_else(|e| panic!("preprocess failed: {e}"));
        if t.kind == TokenKind::Eof {
            break;
        }
        out.push(t.kind);
    }
    out
}

fn int(v: i64) -> TokenKind {
    TokenKind::Int(v)
}
fn id(s: &str) -> TokenKind {
    TokenKind::Ident(s.into())
}

/// Preprocess `src` with `#include` resolved against `dir`.
fn pp_in_dir(src: &str, dir: &std::path::Path) -> Vec<TokenKind> {
    let mut p = Preprocessor::with_base_dir(Lexer::new(src), dir.to_path_buf());
    drain(&mut p)
}

/// Preprocess `src`, resolving angle includes (`#include <name>`) against the
/// `search` directories.
fn pp_search(src: &str, search: Vec<std::path::PathBuf>) -> Vec<TokenKind> {
    let mut p = Preprocessor::with_base_dir_and_search(
        Lexer::new(src),
        std::path::PathBuf::from("."),
        search,
    );
    drain(&mut p)
}

fn drain<S: solomon::lexer::TokenStream>(p: &mut Preprocessor<S>) -> Vec<TokenKind> {
    let mut out = Vec::new();
    loop {
        let t = p
            .next_token()
            .unwrap_or_else(|e| panic!("preprocess failed: {e}"));
        if t.kind == TokenKind::Eof {
            break;
        }
        out.push(t.kind);
    }
    out
}

/// A fresh temp directory unique to this process + `tag` (so parallel tests
/// don't collide), holding the named `(file, contents)` pairs.
fn make_files(tag: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("solomon-pp-{}-{tag}", std::process::id()));
    for sub in files
        .iter()
        .filter_map(|(n, _)| std::path::Path::new(n).parent())
    {
        std::fs::create_dir_all(dir.join(sub)).unwrap();
    }
    std::fs::create_dir_all(&dir).unwrap();
    for (name, contents) in files {
        std::fs::write(dir.join(name), contents).unwrap();
    }
    dir
}

/// The error from preprocessing `src` to completion, or `None` if it succeeds.
fn pp_err(src: &str, dir: &std::path::Path) -> Option<String> {
    let mut p = Preprocessor::with_base_dir(Lexer::new(src), dir.to_path_buf());
    loop {
        match p.next_token() {
            Ok(t) if t.kind == TokenKind::Eof => return None,
            Ok(_) => {}
            Err(e) => return Some(e.message),
        }
    }
}

// ---- object-like macros ----

#[test]
fn object_macro_is_substituted() {
    assert_eq!(
        pp("#define N 3\nN + N"),
        vec![int(3), TokenKind::Plus, int(3)]
    );
}

#[test]
fn object_macro_expands_nested() {
    // A -> B -> 7
    assert_eq!(pp("#define A B\n#define B 7\nA"), vec![int(7)]);
}

#[test]
fn self_referential_macro_does_not_loop() {
    // The hide-set stops `#define X X` from expanding forever.
    assert_eq!(pp("#define X X\nX"), vec![id("X")]);
}

#[test]
fn empty_macro_expands_to_nothing() {
    assert_eq!(pp("#define GONE\nGONE 5"), vec![int(5)]);
}

#[test]
fn undef_removes_a_macro() {
    assert_eq!(pp("#define N 1\n#undef N\nN"), vec![id("N")]);
}

// ---- function-like macros ----

#[test]
fn function_macro_substitutes_arguments() {
    // SQ(2 + 3) -> 2 + 3 * 2 + 3  (body is `a*a`, no parens of its own)
    assert_eq!(
        pp("#define SQ(a) a*a\nSQ(2 + 3)"),
        vec![
            int(2),
            TokenKind::Plus,
            int(3),
            TokenKind::Star,
            int(2),
            TokenKind::Plus,
            int(3)
        ]
    );
}

#[test]
fn function_macro_with_two_params() {
    assert_eq!(
        pp("#define ADD(a, b) a + b\nADD(1, 2)"),
        vec![int(1), TokenKind::Plus, int(2)]
    );
}

#[test]
fn function_macro_name_without_parens_is_literal() {
    // `G` not followed by `(` is just an identifier.
    assert_eq!(
        pp("#define G(x) x\nG + 1"),
        vec![id("G"), TokenKind::Plus, int(1)]
    );
}

#[test]
fn nested_parens_in_arguments() {
    // The argument `(1 + 2)` keeps its inner parens; commas inside parens don't
    // split arguments.
    assert_eq!(
        pp("#define ID(x) x\nID((1, 2))"),
        vec![
            TokenKind::LParen,
            int(1),
            TokenKind::Comma,
            int(2),
            TokenKind::RParen
        ]
    );
}

// ---- conditionals ----

#[test]
fn ifdef_taken_branch() {
    assert_eq!(pp("#define F\n#ifdef F\n1\n#else\n2\n#endif"), vec![int(1)]);
}

#[test]
fn ifdef_else_branch() {
    assert_eq!(pp("#ifdef NOPE\n1\n#else\n2\n#endif"), vec![int(2)]);
}

#[test]
fn ifndef_branch() {
    assert_eq!(pp("#ifndef X\n10\n#endif"), vec![int(10)]);
}

#[test]
fn nested_conditionals() {
    let src = "\
        #define OUTER\n\
        #ifdef OUTER\n\
        1\n\
        #ifdef INNER\n\
        2\n\
        #else\n\
        3\n\
        #endif\n\
        #endif";
    assert_eq!(pp(src), vec![int(1), int(3)]);
}

#[test]
fn define_inside_inactive_branch_is_ignored() {
    // The `#define Y 9` is in a dead branch, so Y stays undefined afterward.
    let src = "#ifdef NOPE\n#define Y 9\n#endif\nY";
    assert_eq!(pp(src), vec![id("Y")]);
}

// ---- pass-through & errors ----

#[test]
fn include_splices_file_tokens() {
    // The included file's tokens stream in, then the parent resumes where the
    // `#include` line left off.
    let dir = make_files("splice", &[("lib.hc", "1 2")]);
    assert_eq!(
        pp_in_dir("#include \"lib.hc\"\n3", &dir),
        vec![int(1), int(2), int(3)]
    );
}

#[test]
fn include_macros_are_visible_after() {
    // A macro defined in an included file expands in the including file.
    let dir = make_files("incmac", &[("def.hc", "#define N 42")]);
    assert_eq!(pp_in_dir("#include \"def.hc\"\nN", &dir), vec![int(42)]);
}

#[test]
fn nested_includes_resolve_relative_to_each_file() {
    let dir = make_files(
        "nested",
        &[
            ("inner.hc", "1"),
            ("sub/outer.hc", "#include \"../inner.hc\"\n2"),
        ],
    );
    assert_eq!(
        pp_in_dir("#include \"sub/outer.hc\"\n3", &dir),
        vec![int(1), int(2), int(3)]
    );
}

#[test]
fn missing_include_errors() {
    let dir = make_files("missing", &[]);
    let err = pp_err("#include \"nope.hc\"\n1", &dir).expect("expected an error");
    assert!(err.contains("cannot open #include"), "got: {err}");
}

#[test]
fn angle_include_resolves_from_the_search_path() {
    // `#include <name>` ignores the including file's directory and resolves
    // against the standard-library search path instead.
    let dir = make_files("angle", &[("math.hc", "#define N 7\nN 8")]);
    assert_eq!(
        pp_search("#include <math.hc>\n9", vec![dir]),
        vec![int(7), int(8), int(9)]
    );
}

#[test]
fn angle_include_tries_search_dirs_in_order() {
    // The first search directory that holds the file wins.
    let a = make_files("angle-a", &[]); // no math.hc here
    let b = make_files("angle-b", &[("math.hc", "5")]);
    assert_eq!(pp_search("#include <math.hc>", vec![a, b]), vec![int(5)]);
}

#[test]
fn angle_include_not_found_errors() {
    let dir = make_files("angle-missing", &[]);
    let mut p = Preprocessor::with_base_dir_and_search(
        Lexer::new("#include <nope.hc>"),
        std::path::PathBuf::from("."),
        vec![dir],
    );
    let err = loop {
        match p.next_token() {
            Ok(t) if t.kind == TokenKind::Eof => panic!("expected an error"),
            Ok(_) => {}
            Err(e) => break e.to_string(),
        }
    };
    assert!(err.contains("cannot find #include <nope.hc>"), "got: {err}");
}

#[test]
fn recursive_include_is_rejected() {
    let dir = make_files(
        "cycle",
        &[("a.hc", "#include \"b.hc\""), ("b.hc", "#include \"a.hc\"")],
    );
    let err = pp_err("#include \"a.hc\"\n1", &dir).expect("expected an error");
    assert!(err.contains("recursive #include"), "got: {err}");
}

#[test]
fn unknown_directive_is_dropped() {
    assert_eq!(pp("#help_index \"X\"\n5"), vec![int(5)]);
}

#[test]
fn unterminated_conditional_errors() {
    let mut p = Preprocessor::new(Lexer::new("#ifdef X\n1"));
    // Reading to the end should surface the missing-#endif error.
    let mut err = None;
    loop {
        match p.next_token() {
            Ok(t) if t.kind == TokenKind::Eof => break,
            Ok(_) => {}
            Err(e) => {
                err = Some(e);
                break;
            }
        }
    }
    assert!(err.is_some(), "expected an unterminated-conditional error");
    assert!(err.unwrap().message.contains("unterminated"));
}

// ---- integration: preprocessing feeds parse + sema ----

#[test]
fn macros_feed_through_to_parsing() {
    // N is used as an array size; SQ as a function-like macro in an expression.
    let src = "\
        #define N 4\n\
        #define SQ(x) ((x) * (x))\n\
        I64 buf[N];\n\
        I64 Area(I64 s) { return SQ(s); }";
    let program = parse(src).expect("should parse");
    assert!(
        check_program(&program).is_empty(),
        "should pass semantic analysis"
    );
}

// ---- type hoisting ----

#[test]
fn forward_reference_to_a_type_parses() {
    // `Thing` is used before it is defined; the hoisting pre-pass makes this
    // parse as a declaration rather than a syntax error.
    let src = "U0 Use() { Thing t; t.id = 1; } class Thing { I64 id; }";
    let program = parse(src).expect("forward type reference should parse");
    assert!(
        check_program(&program).is_empty(),
        "should pass semantic analysis"
    );
}

#[test]
fn forward_reference_without_hoisting_would_be_a_multiply() {
    // Sanity: a name that is NOT a type stays an expression. `Foo * x` with Foo
    // undeclared parses as a multiplication statement (then sema flags Foo).
    let src = "U0 F() { Foo * x; }";
    let program = parse(src).expect("should parse as an expression");
    // Two undeclared identifiers (Foo and x) — proving it was not a declaration.
    let errs = check_program(&program);
    assert!(errs.iter().any(|e| e.message.contains("`Foo`")));
}

#[test]
fn type_defined_in_an_include_is_usable() {
    // Hoisting descends into `#include`d files, so a class declared there parses
    // as a type and the whole program type-checks.
    let dir = make_files("inctype", &[("types.hc", "class Pt { I64 x; I64 y; }")]);
    let src = "#include \"types.hc\"\nU0 Main() { Pt p; p.x = 1; p.y = 2; }";
    let program = parse_in_dir(src, &dir).expect("type from include should parse");
    assert!(
        check_program(&program).is_empty(),
        "should pass semantic analysis"
    );
}

#[test]
fn angle_include_resolves_from_the_embedded_stdlib() {
    // `parse` carries no filesystem search path, so `#include <...>` of a standard
    // library module must resolve from the copy embedded in the compiler at build
    // time — and its types (generic `Vec<T>`) and functions (`StrLen`) become usable.
    let src = "#include <vec.hc>\n#include <cstr.hc>\n\
               U0 Main() { Vec<I64> v; VecInit(&v); I64 n = StrLen(\"hi\"); }";
    let program = parse(src).expect("angle include should resolve from the embedded stdlib");
    assert!(
        check_program(&program).is_empty(),
        "embedded stdlib should type-check: {:?}",
        check_program(&program)
    );
    // A non-stdlib angle include with no search path is still an error.
    assert!(
        parse("#include <does_not_exist.hc>\nU0 Main() {}").is_err(),
        "unknown angle include without a search path should fail"
    );
}
