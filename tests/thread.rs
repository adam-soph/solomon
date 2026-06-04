//! Thread intrinsic tests (`lib/thread.hc`).
//!
//! Threading is impure and concurrent, so these are **property** tests: a program
//! spawns several threads that each compute an independent value, joins them, and
//! prints the per-thread results + their sum. Because each thread writes only its own
//! result (no shared-state race), the concurrent native run and the interpreter's
//! synchronous emulation produce identical stdout — so they double as a conformance
//! check of `Thread`/`Join`.

use std::process::Command;

use solomon::codegen::Codegen;
use solomon::interp::run_to_string;
use solomon::parser::parse_with;
use solomon::sema::check_program;
use solomon::{Arm64Darwin, Arm64Linux, X64Linux};

/// Spawn four threads computing `Square(i)` for i in 2..=5, join them, and print each
/// result and the total. Deterministic stdout regardless of thread interleaving.
const PROGRAM: &str = r#"
    #include <thread.hc>
    I64 Square(I64 x) { return x * x; }
    U0 Main() {
      I64 h[4];
      I64 i;
      for (i = 0; i < 4; i++) h[i] = Thread(&Square, i + 2);
      I64 total = 0;
      for (i = 0; i < 4; i++) {
        I64 r = Join(h[i]);
        "t%d=%d\n", i, r;
        total += r;
      }
      "total=%d\n", total;
    }
    Main;
"#;

// Square(2..=5) = 4, 9, 16, 25; total 54.
const EXPECTED: &str = "t0=4\nt1=9\nt2=16\nt3=25\ntotal=54\n";

fn lib_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lib")
}

fn compile() -> solomon::Program {
    let program = parse_with(PROGRAM, std::path::Path::new("."), &[lib_dir()])
        .unwrap_or_else(|e| panic!("parse failed: {e}"));
    assert!(check_program(&program).is_empty(), "sema errors");
    program
}

#[test]
fn interp_threads() {
    let out = run_to_string(&compile()).unwrap_or_else(|e| panic!("interp error: {e}"));
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

/// `Thread`/`Join` through the **native arm64 Darwin** backend (libc
/// `pthread_create`/`pthread_join`). Self-skips off an Apple-silicon host.
#[test]
fn native_arm64_threads() {
    if !darwin_toolchain() {
        eprintln!("skipping: native thread test needs aarch64-apple-darwin + cc");
        return;
    }
    let bin = std::env::temp_dir().join(format!("solomon-thr-{}", std::process::id()));
    Arm64Darwin::new(&bin)
        .run(&compile())
        .unwrap_or_else(|e| panic!("arm64 build failed: {e}"));
    let output = Command::new(&bin)
        .output()
        .unwrap_or_else(|e| panic!("could not run produced binary: {e}"));
    let _ = std::fs::remove_file(&bin);
    assert_eq!(String::from_utf8_lossy(&output.stdout), EXPECTED);
}

/// Build with `backend` and run the static ELF **natively** (only on a matching Linux
/// host); the threads do `clone(2)` syscalls against the real kernel. Returns stdout.
fn freestanding_thread_stdout(out: &std::path::Path, mut backend: impl Codegen) -> String {
    backend
        .run(&compile())
        .unwrap_or_else(|e| panic!("freestanding build failed: {e}"));
    let output = Command::new(out)
        .output()
        .unwrap_or_else(|e| panic!("could not run produced ELF: {e}"));
    let _ = std::fs::remove_file(out);
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Threads through the **freestanding x86-64** backend — `CLONE_THREAD` via raw
/// `clone(2)` + a futex join. Runs only on a linux/x86_64 host (CI); self-skips
/// elsewhere.
#[test]
fn native_x86_64_freestanding_threads() {
    if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        eprintln!("skipping: freestanding x86-64 thread test needs a linux/x86_64 host");
        return;
    }
    let out = std::env::temp_dir().join(format!("solomon-x64-thr-{}", std::process::id()));
    let got = freestanding_thread_stdout(&out, X64Linux::new(&out));
    assert_eq!(got, EXPECTED, "x86_64 freestanding");
}

/// Threads through the **freestanding aarch64** backend (`CLONE_THREAD` + futex join).
/// Runs only on a linux/aarch64 host; self-skips elsewhere.
#[test]
fn native_arm64_freestanding_threads() {
    if !cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        eprintln!("skipping: freestanding aarch64 thread test needs a linux/aarch64 host");
        return;
    }
    let out = std::env::temp_dir().join(format!("solomon-arm-thr-{}", std::process::id()));
    let got = freestanding_thread_stdout(&out, Arm64Linux::new(&out));
    assert_eq!(got, EXPECTED, "arm64 freestanding");
}
