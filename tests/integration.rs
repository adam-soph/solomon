//! Integration tests: one per `tests/cases/**/*.hc` program.
//!
//! The `test_case!("cases")` proc-macro globs that directory at compile time and emits one
//! `#[test]` per `.hc` file. Each compiles its program on the host target and asserts
//! byte-for-byte stdout parity with the IR-interpreter oracle plus a committed `.out`
//! golden (a host-independent structural check runs on every host). See
//! `tests/support/mod.rs`. (Adding/removing a `.hc` needs a rebuild of this crate to be
//! re-globbed — `touch tests/integration.rs`.)

mod support;
use support::run_case;

// One `#[test]` per `tests/cases/**/*.hc`, generated at compile time by globbing the dir.
hcc_test_macros::test_case!("cases");
