//! File I/O intrinsic tests (`lib/io.hc`).
//!
//! File I/O is impure, so these are **property** tests: a HolyC program writes a
//! known string to a path, reads it back, and prints the content + size. Against a
//! fresh temp file the interpreter and the native backends all produce the same
//! stdout, so they double as a conformance check of `Open`/`LSeek`/`Read`/`Write`/
//! `Close` and the `WriteFile`/`ReadFile`/`FileSize` helpers.

use std::process::Command;

use solomon::codegen::Codegen;
use solomon::interp::run_to_string;
use solomon::parser::parse_with;
use solomon::sema::check_program;
use solomon::{Arm64Darwin, Arm64Linux, X64Linux};

/// A HolyC program that writes `"solomon\n"` to `path`, reads it back, and prints the
/// content and the file size. Deterministic stdout: `got: solomon\nsize=8\n`.
fn file_program(path: &str) -> String {
    format!(
        r#"
        #include <io.hc>
        U0 Main() {{
          U8 *msg = "solomon\n";
          I64 wr = WriteFile("{path}", msg, StrLen(msg));
          if (wr < 0) {{ "write: %s\n", StrError(-wr); return; }}
          U8 buf[64];
          I64 n = ReadFile("{path}", buf, 64);
          if (n < 0) {{ "read: %s\n", StrError(-n); return; }}
          buf[n] = 0;
          "got: %s", buf;
          "size=%d\n", FileSize("{path}");
        }}
        Main;
    "#
    )
}

const EXPECTED: &str = "got: solomon\nsize=8\n";

fn lib_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lib")
}

fn compile(src: &str) -> solomon::Program {
    let program = parse_with(src, std::path::Path::new("."), &[lib_dir()])
        .unwrap_or_else(|e| panic!("parse failed: {e}"));
    assert!(check_program(&program).is_empty(), "sema errors");
    program
}

/// A process-unique temp path for the host-run (interp / Darwin) cases.
fn tmp_path(tag: &str) -> String {
    std::env::temp_dir()
        .join(format!("solomon-io-{tag}-{}.txt", std::process::id()))
        .to_string_lossy()
        .into_owned()
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

/// Reading a nonexistent path fails: the helper returns a negative `-errno`, which
/// `StrError` renders. ENOENT is 2 on both Linux and macOS, and the interpreter and
/// the Darwin backend now both surface the real errno, so the message is identical
/// across targets.
const ERR_PROGRAM: &str = r#"
    #include <io.hc>
    U0 Main() {
      U8 buf[16];
      I64 n = ReadFile("/no/such/solomon/path", buf, 16);
      if (n < 0) "error: %s\n", StrError(-n);
      else "unexpected success\n";
    }
    Main;
"#;

#[test]
fn interp_error_is_reported() {
    let out = run_to_string(&compile(ERR_PROGRAM)).unwrap_or_else(|e| panic!("interp error: {e}"));
    assert_eq!(out, "error: No such file or directory\n");
}

#[test]
fn native_arm64_error_is_reported() {
    if !darwin_toolchain() {
        eprintln!("skipping: native error test needs aarch64-apple-darwin + cc");
        return;
    }
    let bin = std::env::temp_dir().join(format!("solomon-io-err-{}", std::process::id()));
    Arm64Darwin::new(&bin)
        .run(&compile(ERR_PROGRAM))
        .unwrap_or_else(|e| panic!("arm64 build failed: {e}"));
    let output = Command::new(&bin)
        .output()
        .unwrap_or_else(|e| panic!("could not run produced binary: {e}"));
    let _ = std::fs::remove_file(&bin);
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "error: No such file or directory\n"
    );
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

/// The same round-trip through the **native arm64 Darwin** backend: the file
/// primitives lower to libc `open`/`lseek`/`read`/`write`/`close` (with the
/// Linux→macOS open-flag translation). Self-skips off an Apple-silicon host.
#[test]
fn native_arm64_file_roundtrip() {
    if !darwin_toolchain() {
        eprintln!("skipping: native file test needs aarch64-apple-darwin + cc");
        return;
    }
    let path = tmp_path("darwin");
    let _ = std::fs::remove_file(&path);
    let bin = std::env::temp_dir().join(format!("solomon-io-bin-{}", std::process::id()));
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

/// Whether `docker` is usable (the freestanding file tests run the static ELF in a
/// `linux/<arch>` container, writing to the container's own `/tmp`).
fn docker_available() -> bool {
    Command::new("docker")
        .args(["info"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Build `program` with `backend` to a temp ELF and run it under
/// `docker --platform <platform>` (a bare `alpine`; the static ELF needs no libc).
/// The program writes/reads a file in the container's own `/tmp`. Returns stdout.
fn freestanding_file_stdout(
    platform: &str,
    out: &std::path::Path,
    mut backend: impl Codegen,
) -> String {
    backend
        .run(&compile(&file_program("/tmp/solomon_io_test.txt")))
        .unwrap_or_else(|e| panic!("freestanding build failed: {e}"));
    let output = Command::new("docker")
        .args([
            "run",
            "--rm",
            "--platform",
            platform,
            "-v",
            &format!("{}:/prog:ro", out.display()),
            "alpine",
            "/prog",
        ])
        .output()
        .unwrap_or_else(|e| panic!("docker run failed: {e}"));
    let _ = std::fs::remove_file(out);
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// File round-trip through the **freestanding x86-64** backend — raw Linux file
/// syscalls (open/lseek/read/write/close), in a `linux/amd64` container.
#[test]
fn native_x86_64_freestanding_file() {
    if !docker_available() {
        eprintln!("skipping: freestanding file test needs docker");
        return;
    }
    let out = std::env::temp_dir().join(format!("solomon-x64-io-{}", std::process::id()));
    let got = freestanding_file_stdout("linux/amd64", &out, X64Linux::new(&out));
    assert_eq!(got, EXPECTED, "x86_64 freestanding");
}

/// File round-trip through the **freestanding aarch64** backend (raw arm64 Linux
/// syscalls — `openat`+AT_FDCWD, lseek 62), in a `linux/arm64` container.
#[test]
fn native_arm64_freestanding_file() {
    if !docker_available() {
        eprintln!("skipping: freestanding file test needs docker");
        return;
    }
    let out = std::env::temp_dir().join(format!("solomon-arm-io-{}", std::process::id()));
    let got = freestanding_file_stdout("linux/arm64", &out, Arm64Linux::new(&out));
    assert_eq!(got, EXPECTED, "arm64 freestanding");
}
