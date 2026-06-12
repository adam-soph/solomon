//! `System` intrinsic tests (`lib/stdlib.hc`).
//!
//! Running a child process is impure, so these are **property** tests in the style of
//! `tests/io`: the HolyC program shells out three times — a successful `echo` (whose
//! stdout must interleave into the program's own output, before the rc line), an
//! `exit 42`, and a missing command (sh exits 127) — and the stdout is deterministic
//! on every Unix target. The interpreter captures the child's stdout into the
//! program's captured output; native children inherit the real stdout. Unix-only:
//! a Windows host's interpreter shells through `cmd /C`, whose echo emits CRLF.

use std::process::Command;

use hcc::backend::Codegen;
use hcc::oracle::run_to_string;
use hcc::parser::parse_with;
use hcc::sema::check_program;
use hcc::{Arm64Darwin, Arm64Linux, X64Linux};

const PROGRAM: &str = include_str!("system.hc");

const EXPECTED: &str = "before\nfrom-child\nrc0=0\nrc42=42\nrc127=127\nafter\n";

fn lib_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lib")
}

fn compile(src: &str) -> hcc::Program {
    let program = parse_with(src, std::path::Path::new("."), &[lib_dir()])
        .unwrap_or_else(|e| panic!("parse failed: {e}"));
    assert!(check_program(&program).is_empty(), "sema errors");
    program
}

#[cfg(unix)]
#[test]
fn interp_system() {
    let out = run_to_string(&compile(PROGRAM)).unwrap_or_else(|e| panic!("interp error: {e}"));
    assert_eq!(out, EXPECTED);
}

/// Whether this host can build *and execute* an arm64 Mach-O binary.
fn darwin_toolchain() -> bool {
    cfg!(all(target_arch = "aarch64", target_os = "macos"))
        && Command::new("cc")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
}

/// `System` on native arm64 Darwin: libc `system(cmd)` plus the wait-status decode.
#[test]
fn native_arm64_system() {
    if !darwin_toolchain() {
        eprintln!("skipping: native System test needs aarch64-apple-darwin + cc");
        return;
    }
    let bin = std::env::temp_dir().join(format!("hcc-proc-{}", std::process::id()));
    Arm64Darwin::new(&bin)
        .run(&compile(PROGRAM))
        .unwrap_or_else(|e| panic!("arm64 build failed: {e}"));
    let output = Command::new(&bin)
        .output()
        .unwrap_or_else(|e| panic!("could not run produced binary: {e}"));
    let _ = std::fs::remove_file(&bin);
    assert_eq!(String::from_utf8_lossy(&output.stdout), EXPECTED);
}

/// Build `PROGRAM` with `backend` to a temp ELF, run it natively, return stdout.
/// Only called on a matching Linux host; exercises the raw fork/execve/wait4 path.
fn freestanding_stdout(out: &std::path::Path, mut backend: impl Codegen) -> String {
    backend
        .run(&compile(PROGRAM))
        .unwrap_or_else(|e| panic!("freestanding build failed: {e}"));
    let output = Command::new(out)
        .output()
        .unwrap_or_else(|e| panic!("could not run produced ELF: {e}"));
    let _ = std::fs::remove_file(out);
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn native_x86_64_freestanding_system() {
    if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        eprintln!("skipping: freestanding x86-64 System test needs a linux/x86_64 host");
        return;
    }
    let out = std::env::temp_dir().join(format!("hcc-x64-proc-{}", std::process::id()));
    let got = freestanding_stdout(&out, X64Linux::new(&out));
    assert_eq!(got, EXPECTED, "x86_64 freestanding");
}

#[test]
fn native_arm64_freestanding_system() {
    if !cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        eprintln!("skipping: freestanding aarch64 System test needs a linux/aarch64 host");
        return;
    }
    let out = std::env::temp_dir().join(format!("hcc-arm-proc-{}", std::process::id()));
    let got = freestanding_stdout(&out, Arm64Linux::new(&out));
    assert_eq!(got, EXPECTED, "arm64 freestanding");
}
