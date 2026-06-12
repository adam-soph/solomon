//! Unit tests for the oracle's public entry points — `run_to_string`,
//! `run_to_bytes[_with_input]`, `run_to_string_with_input`, and `run_to_bytes_with`.
//! These cover text vs raw-byte capture, the `argv` command line, stdin plumbing, and
//! the semantic-error path. Each lowers and runs a freshly parsed program — the same
//! library path the `tests/cases` corpus and compile-time `#exe` use, now that the
//! oracle is not reachable from the CLI.

use super::*;

/// Parse `src` into a program. These cases all print, so `<stdio.hh>` is prepended
/// (printing needs an explicit include now — there is no auto-include); a duplicate
/// include in the source itself is a guard-deduped no-op.
fn prog(src: &str) -> crate::ast::Program {
    crate::parser::parse(&format!("#include <stdio.hh>\n{src}")).expect("parse")
}

/// Run `src` and capture its stdout as text.
fn out(src: &str) -> String {
    run_to_string(&prog(src)).expect("oracle")
}

/// Run `src` with `args` as its command line (`argv`), capturing stdout as text.
fn out_args(src: &str, args: &[&str]) -> String {
    let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let bytes = run_to_bytes_with(&prog(src), &owned, &[]).expect("oracle");
    String::from_utf8(bytes).expect("utf8 output")
}

#[test]
fn run_to_string_captures_formatted_output() {
    assert_eq!(out(r#""%d\n", 6 * 7;"#), "42\n");
    assert_eq!(out(r#""hello\n";"#), "hello\n");
}

#[test]
fn run_to_bytes_preserves_non_utf8() {
    // `%c` with 0xC8 emits one raw byte that is not valid UTF-8. run_to_bytes keeps it
    // exactly; run_to_string lossily decodes it to U+FFFD, so the two differ.
    let src = r#""%c", 0xC8;"#;
    assert_eq!(run_to_bytes(&prog(src)).unwrap(), vec![0xC8u8]);
    assert_ne!(out(src).into_bytes(), vec![0xC8u8]);
}

#[test]
fn argv_is_the_supplied_command_line() {
    // At top-level scope `argc`/`argv` are the command line passed via `args`.
    assert_eq!(out_args(r#""%d\n", argc;"#, &["prog", "a", "b"]), "3\n");
    assert_eq!(
        out_args(r#""%s\n", argv[1];"#, &["prog", "hello"]),
        "hello\n"
    );
}

#[test]
fn empty_args_default_to_one_argv_entry() {
    // An empty `args` keeps the interpreter's default (`["hcc"]`), so argc == 1.
    assert_eq!(out_args(r#""%d\n", argc;"#, &[]), "1\n");
}

#[test]
fn stdin_reaches_the_program() {
    // Echo stdin byte-for-byte: GetChar returns each byte, then -1 at EOF.
    let echo = r#"#include <stdio.hh>
I64 c;
while ((c = GetChar()) >= 0) "%c", c;"#;
    assert_eq!(
        run_to_string_with_input(&prog(echo), b"hi there").unwrap(),
        "hi there",
    );
    // Raw bytes round-trip too (not just UTF-8 text).
    assert_eq!(
        run_to_bytes_with_input(&prog(echo), &[1u8, 2, 3]).unwrap(),
        vec![1u8, 2, 3],
    );
}

#[test]
fn sema_error_returns_err_not_panic() {
    // The entry point runs semantic analysis first and surfaces the failure as `Err`.
    assert!(run_to_bytes(&prog(r#""%d\n", nonexistent_zq;"#)).is_err());
}
