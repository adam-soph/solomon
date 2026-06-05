//! `_`-directory privacy (Go's `internal/`, generalized): a symbol defined in a file
//! under a directory whose name begins with `_` may be referenced only from files in
//! that directory's *parent* subtree. Enforced in sema; applies to all code — the
//! standard library and user programs alike.

use solomon::parser::{parse, parse_in_dir};
use solomon::sema::check_program;

fn errors(program: &solomon::Program) -> Vec<String> {
    check_program(program)
        .into_iter()
        .map(|e| e.message)
        .collect()
}

/// A program that pulls in `<hmap.hc>` (embedded stdlib) and a body. The stdlib's
/// `hmap.hc` privately uses `Djb2` from `<_impl/strhash.hc>`.
fn stdlib_program(body: &str) -> solomon::Program {
    parse(&format!(
        "#include <hmap.hc>\nU0 Main() {{ {body} }}\nMain;"
    ))
    .unwrap_or_else(|e| panic!("parse failed: {e}"))
}

#[test]
fn public_stdlib_symbol_is_callable() {
    // `HmapStrHash` is public (and bridges to the private `Djb2`) — calling it from
    // user code is fine.
    let p = stdlib_program("U8 *k = \"key\"; HmapStrHash(&k);");
    assert!(errors(&p).is_empty(), "unexpected errors: {:?}", errors(&p));
}

#[test]
fn private_stdlib_symbol_is_rejected_from_user_code() {
    // `Djb2` lives under the stdlib's `_impl/` directory, so a user program may not
    // call it even though `<hmap.hc>` pulled it in transitively.
    let p = stdlib_program("Djb2(\"k\");");
    let errs = errors(&p);
    assert!(
        errs.iter()
            .any(|m| m.contains("Djb2") && m.contains("private")),
        "expected a privacy error for `Djb2`, got: {errs:?}"
    );
}

/// Build a temp project tree and return its root. Layout:
///   <root>/_secret/util.hc   — defines `Secret()` (private to <root>/)
///   <root>/app/main.hc       — in-subtree caller (allowed)
fn temp_project(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("solomon-priv-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("_secret")).unwrap();
    std::fs::create_dir_all(root.join("app")).unwrap();
    std::fs::write(root.join("_secret/util.hc"), "I64 Secret(){ return 42; }\n").unwrap();
    root
}

#[test]
fn private_dir_allows_callers_in_the_parent_subtree() {
    let root = temp_project("ok");
    // app/ is under <root>/, the subtree _secret/ is private to — so this is allowed.
    let src = "#include \"../_secret/util.hc\"\nU0 Main(){ Secret(); }\nMain;";
    let p = parse_in_dir(src, &root.join("app")).unwrap_or_else(|e| panic!("parse: {e}"));
    let errs = errors(&p);
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        errs.is_empty(),
        "in-subtree caller should be allowed: {errs:?}"
    );
}

#[test]
fn private_dir_protects_types_too() {
    // A `class` under a `_` directory is private just like a function.
    let root = std::env::temp_dir().join(format!("solomon-priv-ty-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("_lib")).unwrap();
    std::fs::create_dir_all(root.join("app")).unwrap();
    std::fs::write(root.join("_lib/s.hc"), "class Secret { I64 x; }\n").unwrap();

    let inside = parse_in_dir(
        "#include \"../_lib/s.hc\"\nU0 Main(){ Secret v; v.x = 1; }\nMain;",
        &root.join("app"),
    )
    .unwrap();
    assert!(
        errors(&inside).is_empty(),
        "in-subtree type use should be allowed: {:?}",
        errors(&inside)
    );

    let outside_dir =
        std::env::temp_dir().join(format!("solomon-priv-tyout-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&outside_dir);
    std::fs::create_dir_all(&outside_dir).unwrap();
    // Forward slashes so the absolute path embeds cleanly in the HolyC `#include`
    // string on Windows (`C:\…` would read `\U`/`\T`… as escapes); Windows resolves
    // `/`-separated paths fine.
    let root_inc = root.display().to_string().replace('\\', "/");
    let src =
        format!("#include \"{root_inc}/_lib/s.hc\"\nU0 Main(){{ Secret v; v.x = 1; }}\nMain;");
    let outside = parse_in_dir(&src, &outside_dir).unwrap();
    let errs = errors(&outside);
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&outside_dir);
    assert!(
        errs.iter()
            .any(|m| m.contains("Secret") && m.contains("private")),
        "outside type use should be rejected, got: {errs:?}"
    );
}

#[test]
fn private_dir_rejects_callers_outside_the_subtree() {
    let root = temp_project("bad");
    let outside = std::env::temp_dir().join(format!("solomon-priv-out-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&outside);
    std::fs::create_dir_all(&outside).unwrap();
    // A program outside <root>/ reaching into <root>/_secret/ — rejected. Forward
    // slashes so the absolute path embeds cleanly in the HolyC `#include` on Windows.
    let root_inc = root.display().to_string().replace('\\', "/");
    let src = format!("#include \"{root_inc}/_secret/util.hc\"\nU0 Main(){{ Secret(); }}\nMain;");
    let p = parse_in_dir(&src, &outside).unwrap_or_else(|e| panic!("parse: {e}"));
    let errs = errors(&p);
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&outside);
    assert!(
        errs.iter()
            .any(|m| m.contains("Secret") && m.contains("private")),
        "outside caller should be rejected, got: {errs:?}"
    );
}
