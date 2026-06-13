//! File I/O intrinsic tests (`lib/unistd.hc` + `lib/stdio.hc`).
//!
//! File I/O is impure, so these are **property** tests. A HolyC program writes a
//! known string to a path, reads it back, and prints the content plus size. Against
//! a fresh temp file, the interpreter and all native backends produce the same
//! stdout. So these also serve as a conformance check of `Open`/`LSeek`/`Read`/
//! `Write`/`Close` and the `FileSize` helper.

use std::process::Command;

use hcc::backend::Codegen;
use hcc::oracle::run_to_string;
use hcc::parser::parse_with;
use hcc::sema::check_program;
use hcc::{Arm64Darwin, Arm64Linux, X64Linux};

/// A HolyC program that writes `"hcc\n"` to `path`, reads it back, and prints the
/// content and file size (`tests/io/file.hc`, with `__PATH__` substituted). The stdout
/// is deterministic: `got: hcc\nsize=4\n`.
fn file_program(path: &str) -> String {
    include_str!("file.hc").replace("__PATH__", path)
}

const EXPECTED: &str = "got: hcc\nsize=4\n";

fn lib_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../lib")
}

fn compile(src: &str) -> hcc::Program {
    let program = parse_with(src, std::path::Path::new("."), &[lib_dir()])
        .unwrap_or_else(|e| panic!("parse failed: {e}"));
    assert!(check_program(&program).is_empty(), "sema errors");
    program
}

/// A process-unique temp path for the host-run cases (interp / Darwin). Backslashes
/// are normalized to `/` so the path embeds cleanly in a HolyC string literal on
/// Windows. Otherwise `C:\Users\…` would read `\U…` as an escape. Windows file APIs
/// accept forward slashes.
fn tmp_path(tag: &str) -> String {
    std::env::temp_dir()
        .join(format!("hcc-io-{tag}-{}.txt", std::process::id()))
        .to_string_lossy()
        .replace('\\', "/")
}

#[test]
fn interp_file_roundtrip() {
    let path = tmp_path("interp");
    let _ = std::fs::remove_file(&path);
    let out = run_to_string(&compile(&file_program(&path)))
        .unwrap_or_else(|e| panic!("interp error: {e}"));
    let _ = std::fs::remove_file(&path);
    assert_eq!(out, EXPECTED);
}

/// Reading a nonexistent path fails: the helper returns a negative `-errno`. ENOENT
/// is 2 on both Linux and macOS, and the interpreter and the Darwin backend both
/// surface the real errno, so the number is identical across those targets. Windows
/// reports a different code (`ERROR_PATH_NOT_FOUND` = 3), so the value check below is
/// Unix-only.
const ERR_PROGRAM: &str = include_str!("open_error.hc");

// The exact errno (2/ENOENT) is POSIX-specific; Windows surfaces 3. So pin the value
// on Unix only.
#[cfg(unix)]
#[test]
fn interp_error_is_reported() {
    let out = run_to_string(&compile(ERR_PROGRAM)).unwrap_or_else(|e| panic!("interp error: {e}"));
    assert_eq!(out, "error: errno=2\n");
}

#[test]
fn native_arm64_error_is_reported() {
    if !darwin_toolchain() {
        eprintln!("skipping: native error test needs aarch64-apple-darwin + cc");
        return;
    }
    let bin = std::env::temp_dir().join(format!("hcc-io-err-{}", std::process::id()));
    Arm64Darwin::new(&bin)
        .run(&compile(ERR_PROGRAM))
        .unwrap_or_else(|e| panic!("arm64 build failed: {e}"));
    let output = Command::new(&bin)
        .output()
        .unwrap_or_else(|e| panic!("could not run produced binary: {e}"));
    let _ = std::fs::remove_file(&bin);
    assert_eq!(String::from_utf8_lossy(&output.stdout), "error: errno=2\n");
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

/// The same round-trip through the **native arm64 Darwin** backend. The file
/// primitives lower to libc `open`/`lseek`/`read`/`write`/`close`, with the
/// Linux→macOS open-flag translation. Self-skips off an Apple-silicon host.
#[test]
fn native_arm64_file_roundtrip() {
    if !darwin_toolchain() {
        eprintln!("skipping: native file test needs aarch64-apple-darwin + cc");
        return;
    }
    let path = tmp_path("darwin");
    let _ = std::fs::remove_file(&path);
    let bin = std::env::temp_dir().join(format!("hcc-io-bin-{}", std::process::id()));
    Arm64Darwin::new(&bin)
        .run(&compile(&file_program(&path)))
        .unwrap_or_else(|e| panic!("arm64 build failed: {e}"));
    let output = Command::new(&bin)
        .output()
        .unwrap_or_else(|e| panic!("could not run produced binary: {e}"));
    let _ = std::fs::remove_file(&bin);
    let _ = std::fs::remove_file(&path);
    assert_eq!(String::from_utf8_lossy(&output.stdout), EXPECTED);
}

/// Build `program` with `backend` to a temp ELF and run it **natively**; the static
/// ELF needs no libc. Only called on a matching Linux host. Writes/reads its file in
/// the host's own `/tmp`. Returns stdout.
fn freestanding_file_stdout(out: &std::path::Path, mut backend: impl Codegen) -> String {
    backend
        .run(&compile(&file_program("/tmp/hcc_io_test.txt")))
        .unwrap_or_else(|e| panic!("freestanding build failed: {e}"));
    let output = Command::new(out)
        .output()
        .unwrap_or_else(|e| panic!("could not run produced ELF: {e}"));
    let _ = std::fs::remove_file(out);
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// File round-trip through the **freestanding x86-64** backend, using raw Linux file
/// syscalls (open/lseek/read/write/close). Runs only on a linux/x86_64 host (CI);
/// self-skips elsewhere.
#[test]
fn native_x86_64_freestanding_file() {
    if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        eprintln!("skipping: freestanding x86-64 file test needs a linux/x86_64 host");
        return;
    }
    let out = std::env::temp_dir().join(format!("hcc-x64-io-{}", std::process::id()));
    let got = freestanding_file_stdout(&out, X64Linux::new(&out));
    assert_eq!(got, EXPECTED, "x86_64 freestanding");
}

/// File round-trip through the **freestanding aarch64** backend, using raw arm64
/// Linux syscalls (`openat`+AT_FDCWD, lseek 62). Runs only on a linux/aarch64 host;
/// self-skips elsewhere.
#[test]
fn native_arm64_freestanding_file() {
    if !cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        eprintln!("skipping: freestanding aarch64 file test needs a linux/aarch64 host");
        return;
    }
    let out = std::env::temp_dir().join(format!("hcc-arm-io-{}", std::process::id()));
    let got = freestanding_file_stdout(&out, Arm64Linux::new(&out));
    assert_eq!(got, EXPECTED, "arm64 freestanding");
}

// ---- StdWrite: portable stdout/stderr ----

/// Writes a line to stderr (fd 2, a side channel), then a line to stdout (fd 1), and
/// prints the byte count `StdWrite` returned. The deterministic *stdout* is
/// `stdout line\nwrote=12\n`. The stderr write must NOT appear there.
const STDWRITE_PROGRAM: &str = include_str!("stdwrite.hc");

const STDWRITE_EXPECTED: &str = "stdout line\nwrote=12\n";

#[test]
fn interp_stdwrite() {
    let out =
        run_to_string(&compile(STDWRITE_PROGRAM)).unwrap_or_else(|e| panic!("interp error: {e}"));
    assert_eq!(out, STDWRITE_EXPECTED);
}

/// `StdWrite` on native arm64 Darwin (libc `write` with the fd). Only stdout is
/// captured, so this also confirms the `StdWrite(STDERR, …)` bytes go to fd 2.
#[test]
fn native_arm64_stdwrite() {
    if !darwin_toolchain() {
        eprintln!("skipping: native StdWrite test needs aarch64-apple-darwin + cc");
        return;
    }
    let bin = std::env::temp_dir().join(format!("hcc-stdw-{}", std::process::id()));
    Arm64Darwin::new(&bin)
        .run(&compile(STDWRITE_PROGRAM))
        .unwrap_or_else(|e| panic!("arm64 build failed: {e}"));
    let output = Command::new(&bin)
        .output()
        .unwrap_or_else(|e| panic!("could not run produced binary: {e}"));
    let _ = std::fs::remove_file(&bin);
    assert_eq!(String::from_utf8_lossy(&output.stdout), STDWRITE_EXPECTED);
}

/// `StdWrite` through the **freestanding x86-64** backend (raw `write` syscall).
/// Runs only on a linux/x86_64 host (CI); self-skips elsewhere.
#[test]
fn native_x86_64_freestanding_stdwrite() {
    if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        eprintln!("skipping: freestanding x86-64 StdWrite test needs a linux/x86_64 host");
        return;
    }
    let out = std::env::temp_dir().join(format!("hcc-x64-stdw-{}", std::process::id()));
    let mut backend = X64Linux::new(&out);
    backend
        .run(&compile(STDWRITE_PROGRAM))
        .unwrap_or_else(|e| panic!("freestanding build failed: {e}"));
    let output = Command::new(&out)
        .output()
        .unwrap_or_else(|e| panic!("could not run produced ELF: {e}"));
    let _ = std::fs::remove_file(&out);
    assert_eq!(String::from_utf8_lossy(&output.stdout), STDWRITE_EXPECTED);
}

/// `StdWrite` through the **freestanding aarch64** backend (raw `write` syscall).
/// Runs only on a linux/aarch64 host; self-skips elsewhere.
#[test]
fn native_arm64_freestanding_stdwrite() {
    if !cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        eprintln!("skipping: freestanding aarch64 StdWrite test needs a linux/aarch64 host");
        return;
    }
    let out = std::env::temp_dir().join(format!("hcc-arm-stdw-{}", std::process::id()));
    let mut backend = Arm64Linux::new(&out);
    backend
        .run(&compile(STDWRITE_PROGRAM))
        .unwrap_or_else(|e| panic!("freestanding build failed: {e}"));
    let output = Command::new(&out)
        .output()
        .unwrap_or_else(|e| panic!("could not run produced ELF: {e}"));
    let _ = std::fs::remove_file(&out);
    assert_eq!(String::from_utf8_lossy(&output.stdout), STDWRITE_EXPECTED);
}

// ---- filesystem mutation: Mkdir / Rename / Remove ----

/// A program that creates a directory, writes a file in it, renames it, reads it back,
/// then removes it. The missing-source remove yields -ENOENT = -2 on every target.
fn fsops_program(dir: &str) -> String {
    include_str!("fsops.hc").replace("__DIR__", dir)
}

const FSOPS_EXPECTED: &str = "mkdir=0\nwrite=0\nrename=0\nread=3 got=hi\nrm_missing=-2\nrm=0\n";

/// A process-unique directory path for the host-run cases (interp / Darwin).
/// Backslashes are normalized to `/` so the path embeds cleanly in a HolyC string
/// literal on Windows (see [`tmp_path`]).
fn tmp_dir(tag: &str) -> String {
    std::env::temp_dir()
        .join(format!("hcc-fsops-{tag}-{}", std::process::id()))
        .to_string_lossy()
        .replace('\\', "/")
}

#[test]
fn interp_fsops_roundtrip() {
    let dir = tmp_dir("interp");
    let _ = std::fs::remove_dir_all(&dir);
    let out = run_to_string(&compile(&fsops_program(&dir)))
        .unwrap_or_else(|e| panic!("interp error: {e}"));
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(out, FSOPS_EXPECTED);
}

/// Native arm64 Darwin: Mkdir/Rename/Remove lower to libc `mkdir`/`rename`/`unlink`,
/// with the `-1`→`-errno` conversion. Self-skips off an Apple-silicon host.
#[test]
fn native_arm64_fsops() {
    if !darwin_toolchain() {
        eprintln!("skipping: native fsops test needs aarch64-apple-darwin + cc");
        return;
    }
    let dir = tmp_dir("darwin");
    let _ = std::fs::remove_dir_all(&dir);
    let bin = std::env::temp_dir().join(format!("hcc-fsops-bin-{}", std::process::id()));
    Arm64Darwin::new(&bin)
        .run(&compile(&fsops_program(&dir)))
        .unwrap_or_else(|e| panic!("arm64 build failed: {e}"));
    let output = Command::new(&bin)
        .output()
        .unwrap_or_else(|e| panic!("could not run produced binary: {e}"));
    let _ = std::fs::remove_file(&bin);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(String::from_utf8_lossy(&output.stdout), FSOPS_EXPECTED);
}

/// Build the fsops program with `backend` to a temp ELF and run it natively, using a
/// clean fixed dir. Only called on a matching Linux host. Returns stdout.
fn freestanding_fsops_stdout(out: &std::path::Path, mut backend: impl Codegen) -> String {
    let dir = "/tmp/hcc_fsops_test";
    let _ = std::fs::remove_dir_all(dir);
    backend
        .run(&compile(&fsops_program(dir)))
        .unwrap_or_else(|e| panic!("freestanding build failed: {e}"));
    let output = Command::new(out)
        .output()
        .unwrap_or_else(|e| panic!("could not run produced ELF: {e}"));
    let _ = std::fs::remove_file(out);
    let _ = std::fs::remove_dir_all(dir);
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn native_x86_64_freestanding_fsops() {
    if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        eprintln!("skipping: freestanding x86-64 fsops test needs a linux/x86_64 host");
        return;
    }
    let out = std::env::temp_dir().join(format!("hcc-x64-fsops-{}", std::process::id()));
    let got = freestanding_fsops_stdout(&out, X64Linux::new(&out));
    assert_eq!(got, FSOPS_EXPECTED, "x86_64 freestanding");
}

#[test]
fn native_arm64_freestanding_fsops() {
    if !cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        eprintln!("skipping: freestanding aarch64 fsops test needs a linux/aarch64 host");
        return;
    }
    let out = std::env::temp_dir().join(format!("hcc-arm-fsops-{}", std::process::id()));
    let got = freestanding_fsops_stdout(&out, Arm64Linux::new(&out));
    assert_eq!(got, FSOPS_EXPECTED, "arm64 freestanding");
}

// ---- the environment: envp ----

/// `envp` is a NULL-terminated `U8 **` of "KEY=VALUE" strings. These are structural
/// invariants that hold for any real process environment: it is non-empty and every
/// entry has a '='. So the check is deterministic without depending on a specific
/// variable.
const ENV_INVARIANTS: &str = include_str!("env_invariants.hc");

#[test]
fn interp_envp_invariants() {
    let out = run_to_string(&compile(ENV_INVARIANTS)).unwrap_or_else(|e| panic!("interp: {e}"));
    assert_eq!(out, "nonempty=1 all_kv=1\n");
}

#[test]
fn native_arm64_envp_invariants_and_lookup() {
    if !darwin_toolchain() {
        eprintln!("skipping: native envp test needs aarch64-apple-darwin + cc");
        return;
    }
    // Same invariants, captured natively from `main`'s envp (x2).
    let bin = std::env::temp_dir().join(format!("hcc-env-inv-{}", std::process::id()));
    Arm64Darwin::new(&bin)
        .run(&compile(ENV_INVARIANTS))
        .unwrap_or_else(|e| panic!("arm64 build failed: {e}"));
    let out = Command::new(&bin)
        .output()
        .unwrap_or_else(|e| panic!("run: {e}"));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "nonempty=1 all_kv=1\n"
    );

    // Value lookup of a specific variable, passed to the child's environment. The
    // variable is scoped to the spawned process, so there's no parallel-test race on
    // the shared env.
    let lookup = include_str!("env_lookup.hc");
    let bin2 = std::env::temp_dir().join(format!("hcc-env-look-{}", std::process::id()));
    Arm64Darwin::new(&bin2)
        .run(&compile(lookup))
        .unwrap_or_else(|e| panic!("arm64 build failed: {e}"));
    let out2 = Command::new(&bin2)
        .env("HCC_ENV", "hi")
        .output()
        .unwrap_or_else(|e| panic!("run: {e}"));
    let _ = std::fs::remove_file(&bin);
    let _ = std::fs::remove_file(&bin2);
    assert_eq!(String::from_utf8_lossy(&out2.stdout), "got=hi\n");
}

/// envp invariants through the **freestanding x86-64** backend. Here envp is read off
/// the initial stack, just past argv's NULL. Runs only on a linux/x86_64 host.
#[test]
fn native_x86_64_freestanding_envp() {
    if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        eprintln!("skipping: freestanding x86-64 envp test needs a linux/x86_64 host");
        return;
    }
    let out = std::env::temp_dir().join(format!("hcc-x64-env-{}", std::process::id()));
    X64Linux::new(&out)
        .run(&compile(ENV_INVARIANTS))
        .unwrap_or_else(|e| panic!("freestanding build failed: {e}"));
    let got = Command::new(&out)
        .output()
        .unwrap_or_else(|e| panic!("run: {e}"));
    let _ = std::fs::remove_file(&out);
    assert_eq!(
        String::from_utf8_lossy(&got.stdout),
        "nonempty=1 all_kv=1\n"
    );
}

// ---- Getenv (os.hc) ----

const GETENV_UNSET: &str = include_str!("getenv_unset.hc");

const GETENV_LOOKUP: &str = include_str!("getenv_lookup.hc");

#[test]
fn interp_getenv_unset_is_null() {
    // Race-free: an unset name is NULL regardless of the ambient environment.
    let out = run_to_string(&compile(GETENV_UNSET)).unwrap_or_else(|e| panic!("interp: {e}"));
    assert_eq!(out, "unset=1\n");
}

#[test]
fn native_arm64_getenv_lookup() {
    if !darwin_toolchain() {
        eprintln!("skipping: native Getenv test needs aarch64-apple-darwin + cc");
        return;
    }
    let bin = std::env::temp_dir().join(format!("hcc-getenv-{}", std::process::id()));
    Arm64Darwin::new(&bin)
        .run(&compile(GETENV_LOOKUP))
        .unwrap_or_else(|e| panic!("arm64 build failed: {e}"));
    // Found. The value is scoped to the child, so there's no parallel-test env race.
    let hit = Command::new(&bin)
        .env("HCC_ENV", "world")
        .output()
        .unwrap_or_else(|e| panic!("run: {e}"));
    assert_eq!(String::from_utf8_lossy(&hit.stdout), "got=world\n");
    // Missing.
    let miss = Command::new(&bin)
        .env_remove("HCC_ENV")
        .output()
        .unwrap_or_else(|e| panic!("run: {e}"));
    let _ = std::fs::remove_file(&bin);
    assert_eq!(String::from_utf8_lossy(&miss.stdout), "missing\n");
}

#[test]
fn native_x86_64_freestanding_getenv() {
    if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        eprintln!("skipping: freestanding x86-64 Getenv test needs a linux/x86_64 host");
        return;
    }
    let out = std::env::temp_dir().join(format!("hcc-x64-getenv-{}", std::process::id()));
    X64Linux::new(&out)
        .run(&compile(GETENV_LOOKUP))
        .unwrap_or_else(|e| panic!("freestanding build failed: {e}"));
    let got = Command::new(&out)
        .env("HCC_ENV", "world")
        .output()
        .unwrap_or_else(|e| panic!("run: {e}"));
    let _ = std::fs::remove_file(&out);
    assert_eq!(String::from_utf8_lossy(&got.stdout), "got=world\n");
}

// ---- Getpid (os.hc) ----

const GETPID_PROG: &str = include_str!("getpid.hc");

#[test]
fn interp_getpid_is_positive() {
    let out = run_to_string(&compile(GETPID_PROG)).unwrap_or_else(|e| panic!("interp: {e}"));
    assert_eq!(out, "pos=1\n");
}

#[test]
fn native_arm64_getpid_is_positive() {
    if !darwin_toolchain() {
        eprintln!("skipping: native Getpid test needs aarch64-apple-darwin + cc");
        return;
    }
    let bin = std::env::temp_dir().join(format!("hcc-getpid-{}", std::process::id()));
    Arm64Darwin::new(&bin)
        .run(&compile(GETPID_PROG))
        .unwrap_or_else(|e| panic!("arm64 build failed: {e}"));
    let out = Command::new(&bin)
        .output()
        .unwrap_or_else(|e| panic!("run: {e}"));
    let _ = std::fs::remove_file(&bin);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "pos=1\n");
}

#[test]
fn native_x86_64_freestanding_getpid() {
    if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        eprintln!("skipping: freestanding x86-64 Getpid test needs a linux/x86_64 host");
        return;
    }
    let out = std::env::temp_dir().join(format!("hcc-x64-getpid-{}", std::process::id()));
    X64Linux::new(&out)
        .run(&compile(GETPID_PROG))
        .unwrap_or_else(|e| panic!("freestanding build failed: {e}"));
    let got = Command::new(&out)
        .output()
        .unwrap_or_else(|e| panic!("run: {e}"));
    let _ = std::fs::remove_file(&out);
    assert_eq!(String::from_utf8_lossy(&got.stdout), "pos=1\n");
}

// ---- Chdir / Getcwd (os.hc) ----

// Read-only invariants: Getcwd succeeds into an absolute path, and a bad Chdir fails.
// There's no successful Chdir, so the interpreter's process cwd is not mutated, making
// this race-free under parallel tests. Used only by the Unix-only invariants test (the
// `/`-prefix check).
#[cfg(unix)]
const CWD_INVARIANTS: &str = include_str!("cwd_invariants.hc");

// Deterministic value check: Chdir to root, then Getcwd reports exactly "/". Run in an
// isolated child so the cwd change can't leak.
const CWD_ROOT: &str = include_str!("cwd_root.hc");

// The `abs` check is `buf[0] == '/'`, i.e. a POSIX absolute path. On Windows the cwd is
// absolute but drive-rooted (`C:\…`), so this invariant is Unix-only.
#[cfg(unix)]
#[test]
fn interp_getcwd_invariants() {
    let out = run_to_string(&compile(CWD_INVARIANTS)).unwrap_or_else(|e| panic!("interp: {e}"));
    assert_eq!(out, "getcwd=0 abs=1 badchdir=1\n");
}

#[test]
fn native_arm64_chdir_getcwd() {
    if !darwin_toolchain() {
        eprintln!("skipping: native Chdir/Getcwd test needs aarch64-apple-darwin + cc");
        return;
    }
    let bin = std::env::temp_dir().join(format!("hcc-cwd-{}", std::process::id()));
    Arm64Darwin::new(&bin)
        .run(&compile(CWD_ROOT))
        .unwrap_or_else(|e| panic!("arm64 build failed: {e}"));
    let out = Command::new(&bin)
        .output()
        .unwrap_or_else(|e| panic!("run: {e}"));
    let _ = std::fs::remove_file(&bin);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "chdir=0\ncwd=/\n");
}

#[test]
fn native_x86_64_freestanding_chdir_getcwd() {
    if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        eprintln!("skipping: freestanding x86-64 Chdir/Getcwd test needs a linux/x86_64 host");
        return;
    }
    let out = std::env::temp_dir().join(format!("hcc-x64-cwd-{}", std::process::id()));
    X64Linux::new(&out)
        .run(&compile(CWD_ROOT))
        .unwrap_or_else(|e| panic!("freestanding build failed: {e}"));
    let got = Command::new(&out)
        .output()
        .unwrap_or_else(|e| panic!("run: {e}"));
    let _ = std::fs::remove_file(&out);
    assert_eq!(String::from_utf8_lossy(&got.stdout), "chdir=0\ncwd=/\n");
}

// ---- Getppid / Getuid (os.hc) ----

const IDS_PROG: &str = include_str!("ids.hc");

// Asserts POSIX id semantics (a real parent pid `> 0`). On Windows the interpreter has
// no POSIX ppid/uid/gid (they report 0), so this is a Unix-only check.
#[cfg(unix)]
#[test]
fn interp_getppid_getuid_are_sane() {
    let out = run_to_string(&compile(IDS_PROG)).unwrap_or_else(|e| panic!("interp: {e}"));
    assert_eq!(out, "ppid=1 uid=1 gid=1\n");
}

#[test]
fn native_arm64_getppid_getuid_are_sane() {
    if !darwin_toolchain() {
        eprintln!("skipping: native id test needs aarch64-apple-darwin + cc");
        return;
    }
    let bin = std::env::temp_dir().join(format!("hcc-ids-{}", std::process::id()));
    Arm64Darwin::new(&bin)
        .run(&compile(IDS_PROG))
        .unwrap_or_else(|e| panic!("arm64 build failed: {e}"));
    let out = Command::new(&bin)
        .output()
        .unwrap_or_else(|e| panic!("run: {e}"));
    let _ = std::fs::remove_file(&bin);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "ppid=1 uid=1 gid=1\n");
}

#[test]
fn native_x86_64_freestanding_getppid_getuid() {
    if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        eprintln!("skipping: freestanding x86-64 id test needs a linux/x86_64 host");
        return;
    }
    let out = std::env::temp_dir().join(format!("hcc-x64-ids-{}", std::process::id()));
    X64Linux::new(&out)
        .run(&compile(IDS_PROG))
        .unwrap_or_else(|e| panic!("freestanding build failed: {e}"));
    let got = Command::new(&out)
        .output()
        .unwrap_or_else(|e| panic!("run: {e}"));
    let _ = std::fs::remove_file(&out);
    assert_eq!(String::from_utf8_lossy(&got.stdout), "ppid=1 uid=1 gid=1\n");
}

// ---- Environ (os.hc) ----

const ENVIRON_PROG: &str = include_str!("environ.hc");

#[test]
fn interp_environ_collects_entries() {
    let out = run_to_string(&compile(ENVIRON_PROG)).unwrap_or_else(|e| panic!("interp: {e}"));
    assert_eq!(out, "count>0=1 all_kv=1\n");
}

#[test]
fn native_arm64_environ_collects_entries() {
    if !darwin_toolchain() {
        eprintln!("skipping: native Environ test needs aarch64-apple-darwin + cc");
        return;
    }
    let bin = std::env::temp_dir().join(format!("hcc-environ-{}", std::process::id()));
    Arm64Darwin::new(&bin)
        .run(&compile(ENVIRON_PROG))
        .unwrap_or_else(|e| panic!("arm64 build failed: {e}"));
    let out = Command::new(&bin)
        .output()
        .unwrap_or_else(|e| panic!("run: {e}"));
    let _ = std::fs::remove_file(&bin);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "count>0=1 all_kv=1\n");
}

#[test]
fn native_x86_64_freestanding_environ() {
    if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        eprintln!("skipping: freestanding x86-64 Environ test needs a linux/x86_64 host");
        return;
    }
    let out = std::env::temp_dir().join(format!("hcc-x64-environ-{}", std::process::id()));
    X64Linux::new(&out)
        .run(&compile(ENVIRON_PROG))
        .unwrap_or_else(|e| panic!("freestanding build failed: {e}"));
    let got = Command::new(&out)
        .output()
        .unwrap_or_else(|e| panic!("run: {e}"));
    let _ = std::fs::remove_file(&out);
    assert_eq!(String::from_utf8_lossy(&got.stdout), "count>0=1 all_kv=1\n");
}
