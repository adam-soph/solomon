//! Thread intrinsic tests (`lib/thread.hc`).
//!
//! Threading is impure and concurrent, so these are **property** tests. A program
//! spawns several threads that each compute an independent value, joins them, and
//! prints the per-thread results plus their sum. Each thread writes only its own
//! result, so there's no shared-state race. As a result, the concurrent native run and
//! the interpreter's synchronous emulation produce identical stdout, doubling as a
//! conformance check of `Thread`/`Join`.

use std::process::Command;

use hcc::backend::Codegen;
use hcc::oracle::run_to_string;
use hcc::parser::parse_with;
use hcc::sema::check_program;
use hcc::{Arm64Darwin, Arm64Linux, X64Linux};

/// Spawn four threads computing `Square(i)` for i in 2..=5, join them, and print each
/// result and the total. The stdout is deterministic regardless of thread interleaving.
const PROGRAM: &str = include_str!("squares.hc");

// Square(2..=5) = 4, 9, 16, 25; total 54.
const EXPECTED: &str = "t0=4\nt1=9\nt2=16\nt3=25\ntotal=54\n";

/// Per-thread exception state: each worker throws and catches its own value, returning
/// it through `Join`. Exception state (`Fs`) is thread-local, so concurrent throws never
/// race — a shared global would corrupt the caught values. Deterministic regardless of
/// interleaving (results are reported in join order).
const EXC_PROGRAM: &str = include_str!("exceptions.hc");

const EXC_EXPECTED: &str = "w0=100\nw1=200\nw2=300\nw3=400\n";

fn lib_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lib")
}

fn compile_src(src: &str) -> hcc::Program {
    let program = parse_with(src, std::path::Path::new("."), &[lib_dir()])
        .unwrap_or_else(|e| panic!("parse failed: {e}"));
    assert!(check_program(&program).is_empty(), "sema errors");
    program
}

fn compile() -> hcc::Program {
    compile_src(PROGRAM)
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
    let bin = std::env::temp_dir().join(format!("hcc-thr-{}", std::process::id()));
    Arm64Darwin::new(&bin)
        .run(&compile())
        .unwrap_or_else(|e| panic!("arm64 build failed: {e}"));
    let output = Command::new(&bin)
        .output()
        .unwrap_or_else(|e| panic!("could not run produced binary: {e}"));
    let _ = std::fs::remove_file(&bin);
    assert_eq!(String::from_utf8_lossy(&output.stdout), EXPECTED);
}

/// Build with `backend` and run the static ELF **natively**. Only called on a matching
/// Linux host. The threads do `clone(2)` syscalls against the real kernel. Returns
/// stdout.
fn freestanding_thread_stdout(out: &std::path::Path, mut backend: impl Codegen) -> String {
    freestanding_stdout(out, &mut backend, &compile())
}

/// Build `program` with `backend` into `out` and run the static ELF, returning stdout.
fn freestanding_stdout(
    out: &std::path::Path,
    backend: &mut impl Codegen,
    program: &hcc::Program,
) -> String {
    backend
        .run(program)
        .unwrap_or_else(|e| panic!("freestanding build failed: {e}"));
    let output = Command::new(out)
        .output()
        .unwrap_or_else(|e| panic!("could not run produced ELF: {e}"));
    let _ = std::fs::remove_file(out);
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Threads through the **freestanding x86-64** backend: `CLONE_THREAD` via raw
/// `clone(2)`, with a futex join. Runs only on a linux/x86_64 host (CI); self-skips
/// elsewhere.
#[test]
fn native_x86_64_freestanding_threads() {
    if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        eprintln!("skipping: freestanding x86-64 thread test needs a linux/x86_64 host");
        return;
    }
    let out = std::env::temp_dir().join(format!("hcc-x64-thr-{}", std::process::id()));
    let got = freestanding_thread_stdout(&out, X64Linux::new(&out));
    assert_eq!(got, EXPECTED, "x86_64 freestanding");
}

/// Threads through the **freestanding aarch64** backend (`CLONE_THREAD` plus a futex
/// join). Runs only on a linux/aarch64 host; self-skips elsewhere.
#[test]
fn native_arm64_freestanding_threads() {
    if !cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        eprintln!("skipping: freestanding aarch64 thread test needs a linux/aarch64 host");
        return;
    }
    let out = std::env::temp_dir().join(format!("hcc-arm-thr-{}", std::process::id()));
    let got = freestanding_thread_stdout(&out, Arm64Linux::new(&out));
    assert_eq!(got, EXPECTED, "arm64 freestanding");
}

// ---- per-thread exception state (Fs is thread-local) ----

#[test]
fn interp_exceptions_threads() {
    let out =
        run_to_string(&compile_src(EXC_PROGRAM)).unwrap_or_else(|e| panic!("interp error: {e}"));
    assert_eq!(out, EXC_EXPECTED);
}

/// Per-thread exception state through the **native arm64 Darwin** backend (pthread TLS).
/// Self-skips off an Apple-silicon host.
#[test]
fn native_arm64_exceptions_threads() {
    if !darwin_toolchain() {
        eprintln!("skipping: native thread test needs aarch64-apple-darwin + cc");
        return;
    }
    let bin = std::env::temp_dir().join(format!("hcc-exthr-{}", std::process::id()));
    Arm64Darwin::new(&bin)
        .run(&compile_src(EXC_PROGRAM))
        .unwrap_or_else(|e| panic!("arm64 build failed: {e}"));
    let output = Command::new(&bin)
        .output()
        .unwrap_or_else(|e| panic!("could not run produced binary: {e}"));
    let _ = std::fs::remove_file(&bin);
    assert_eq!(String::from_utf8_lossy(&output.stdout), EXC_EXPECTED);
}

/// Per-thread exception state through the **freestanding x86-64** backend (`%fs` TLS via
/// the `clone` `CLONE_SETTLS`). Runs only on a linux/x86_64 host (CI).
#[test]
fn native_x86_64_freestanding_exceptions() {
    if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        eprintln!("skipping: freestanding x86-64 thread test needs a linux/x86_64 host");
        return;
    }
    let out = std::env::temp_dir().join(format!("hcc-x64-exthr-{}", std::process::id()));
    let got = freestanding_stdout(&out, &mut X64Linux::new(&out), &compile_src(EXC_PROGRAM));
    assert_eq!(got, EXC_EXPECTED, "x86_64 freestanding exceptions");
}

/// Per-thread exception state through the **freestanding aarch64** backend (`TPIDR_EL0`
/// set in the `clone` child). Runs only on a linux/aarch64 host (CI).
#[test]
fn native_arm64_freestanding_exceptions() {
    if !cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        eprintln!("skipping: freestanding aarch64 thread test needs a linux/aarch64 host");
        return;
    }
    let out = std::env::temp_dir().join(format!("hcc-arm-exthr-{}", std::process::id()));
    let got = freestanding_stdout(&out, &mut Arm64Linux::new(&out), &compile_src(EXC_PROGRAM));
    assert_eq!(got, EXC_EXPECTED, "arm64 freestanding exceptions");
}

// ---- TLS (`TssCreate`/`TssSet`/`TssGet`) + `CallOnce` + `ThreadYield` ----

/// Each worker round-trips its own value through a shared TLS key (`Gettid`-keyed, so
/// a thread never reads another's write) and `CallOnce` runs its body exactly once
/// across all of them. Join-order output, deterministic on every engine — including
/// the synchronous interpreter, where all workers share the main tid but still each
/// read back the value they just wrote.
const TLS_PROGRAM: &str = include_str!("tls.hc");

const TLS_EXPECTED: &str = "w0=100\nw1=200\nw2=300\nonce=1\n";

#[test]
fn interp_tls() {
    let out =
        run_to_string(&compile_src(TLS_PROGRAM)).unwrap_or_else(|e| panic!("interp error: {e}"));
    assert_eq!(out, TLS_EXPECTED);
}

/// TLS + CallOnce on native arm64 Darwin (pthreads; `Gettid` via
/// `pthread_threadid_np`, so each worker has a genuinely distinct tid).
#[test]
fn native_arm64_tls() {
    if !darwin_toolchain() {
        eprintln!("skipping: native TLS test needs aarch64-apple-darwin + cc");
        return;
    }
    let bin = std::env::temp_dir().join(format!("hcc-tlsthr-{}", std::process::id()));
    Arm64Darwin::new(&bin)
        .run(&compile_src(TLS_PROGRAM))
        .unwrap_or_else(|e| panic!("arm64 build failed: {e}"));
    let output = Command::new(&bin)
        .output()
        .unwrap_or_else(|e| panic!("could not run produced binary: {e}"));
    let _ = std::fs::remove_file(&bin);
    assert_eq!(String::from_utf8_lossy(&output.stdout), TLS_EXPECTED);
}

/// TLS + CallOnce through the **freestanding x86-64** backend (`gettid` syscall per
/// clone(2) thread). Runs only on a linux/x86_64 host (CI).
#[test]
fn native_x86_64_freestanding_tls() {
    if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        eprintln!("skipping: freestanding x86-64 TLS test needs a linux/x86_64 host");
        return;
    }
    let out = std::env::temp_dir().join(format!("hcc-x64-tls-{}", std::process::id()));
    let got = freestanding_stdout(&out, &mut X64Linux::new(&out), &compile_src(TLS_PROGRAM));
    assert_eq!(got, TLS_EXPECTED, "x86_64 freestanding TLS");
}

/// TLS + CallOnce through the **freestanding aarch64** backend. Runs only on a
/// linux/aarch64 host (CI).
#[test]
fn native_arm64_freestanding_tls() {
    if !cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        eprintln!("skipping: freestanding aarch64 TLS test needs a linux/aarch64 host");
        return;
    }
    let out = std::env::temp_dir().join(format!("hcc-arm-tls-{}", std::process::id()));
    let got = freestanding_stdout(&out, &mut Arm64Linux::new(&out), &compile_src(TLS_PROGRAM));
    assert_eq!(got, TLS_EXPECTED, "arm64 freestanding TLS");
}

// ---- `ThreadExit` ----

/// `ThreadExit(ret)` inside a body is the join value; a plain return still works; a
/// main-flow `ThreadExit(9)` ends the program (so "unreachable" never prints) with
/// status 9 — asserted on the native legs (the interpreter run only sees stdout).
const TEXIT_PROGRAM: &str = include_str!("thread_exit.hc");

const TEXIT_EXPECTED: &str = "j1=100\nj2=5\ndone\n";

#[test]
fn interp_thread_exit() {
    let out =
        run_to_string(&compile_src(TEXIT_PROGRAM)).unwrap_or_else(|e| panic!("interp error: {e}"));
    assert_eq!(out, TEXIT_EXPECTED);
}

#[test]
fn native_arm64_thread_exit() {
    if !darwin_toolchain() {
        eprintln!("skipping: native ThreadExit test needs aarch64-apple-darwin + cc");
        return;
    }
    let bin = std::env::temp_dir().join(format!("hcc-texit-{}", std::process::id()));
    Arm64Darwin::new(&bin)
        .run(&compile_src(TEXIT_PROGRAM))
        .unwrap_or_else(|e| panic!("arm64 build failed: {e}"));
    let output = Command::new(&bin)
        .output()
        .unwrap_or_else(|e| panic!("could not run produced binary: {e}"));
    let _ = std::fs::remove_file(&bin);
    assert_eq!(String::from_utf8_lossy(&output.stdout), TEXIT_EXPECTED);
    assert_eq!(output.status.code(), Some(9), "main-flow ThreadExit status");
}

/// `ThreadExit` through the **freestanding x86-64** backend (`arch_prctl` fs-base
/// read + control-block store). Runs only on a linux/x86_64 host (CI).
#[test]
fn native_x86_64_freestanding_thread_exit() {
    if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        eprintln!("skipping: freestanding x86-64 ThreadExit test needs a linux/x86_64 host");
        return;
    }
    let out = std::env::temp_dir().join(format!("hcc-x64-texit-{}", std::process::id()));
    let mut backend = X64Linux::new(&out);
    backend
        .run(&compile_src(TEXIT_PROGRAM))
        .unwrap_or_else(|e| panic!("freestanding build failed: {e}"));
    let output = Command::new(&out)
        .output()
        .unwrap_or_else(|e| panic!("could not run produced ELF: {e}"));
    let _ = std::fs::remove_file(&out);
    assert_eq!(String::from_utf8_lossy(&output.stdout), TEXIT_EXPECTED);
    assert_eq!(output.status.code(), Some(9), "main-flow ThreadExit status");
}

/// `ThreadExit` through the **freestanding aarch64** backend (TPIDR_EL0 via
/// `CLONE_SETTLS` + control-block store). Runs only on a linux/aarch64 host (CI).
#[test]
fn native_arm64_freestanding_thread_exit() {
    if !cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        eprintln!("skipping: freestanding aarch64 ThreadExit test needs a linux/aarch64 host");
        return;
    }
    let out = std::env::temp_dir().join(format!("hcc-arm-texit-{}", std::process::id()));
    let mut backend = Arm64Linux::new(&out);
    backend
        .run(&compile_src(TEXIT_PROGRAM))
        .unwrap_or_else(|e| panic!("freestanding build failed: {e}"));
    let output = Command::new(&out)
        .output()
        .unwrap_or_else(|e| panic!("could not run produced ELF: {e}"));
    let _ = std::fs::remove_file(&out);
    assert_eq!(String::from_utf8_lossy(&output.stdout), TEXIT_EXPECTED);
    assert_eq!(output.status.code(), Some(9), "main-flow ThreadExit status");
}
