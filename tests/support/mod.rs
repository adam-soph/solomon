//! Shared harness for the integration and comparison test crates.
//!
//! This is a *subdirectory* module (`tests/support/mod.rs`), so Cargo does **not**
//! compile it as its own test binary; it is pulled in with `mod support;` by
//! `tests/integration.rs` and `tests/comparison.rs`.
//!
//! The contract every integration case upholds: a HolyC program compiled on the host
//! target produces **byte-for-byte** the same stdout as the IR interpreter (the
//! conformance oracle, [`hcc::irinterp`]). Two layers, mirroring the old per-backend
//! suites:
//!
//! - **Structural** ([`structural_macho`]) — emit the AArch64 Mach-O object and byte-check
//!   its container. This goes through [`Arm64Darwin::object`], which needs no `cc` and no
//!   execution, so it runs on **every** host and exercises the emitter even off-target.
//! - **End-to-end** ([`build_and_run_native`]) — build the *host* target's binary, run it,
//!   and compare stdout to the oracle. Self-skips off a runnable host (or when
//!   `SOLOMON_SKIP_NATIVE` is set), so a green run on one host is structural-only for the
//!   other targets; the union of CI legs execs every backend.

#![allow(dead_code)] // each test crate uses a different subset of these helpers.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use hcc::backend::Codegen;
use hcc::irinterp::run_to_bytes_with;
use hcc::parser::parse_with;
use hcc::sema::check_program;
use hcc::{Arm64Darwin, Program};

static COUNTER: AtomicU32 = AtomicU32::new(0);

// ---------------------------------------------------------------------------
// Case directives — a tiny header convention for the few cases needing argv/stdin.
// Only leading `//@ ...` lines are read; scanning stops at the first other line.
// ---------------------------------------------------------------------------

/// The program arguments (`argv[1..]`, *not* `argv[0]`) and stdin bytes a case requests,
/// plus the optional error-path directive.
pub struct Directives {
    pub args: Vec<String>,
    pub stdin: Vec<u8>,
    /// `//@ error: <substring>` — flips the case into a *rejection* check: the front end
    /// must fail to compile the program with a message containing this substring (no golden,
    /// no native run). The empty string matches any error (just "this must not compile").
    pub error: Option<String>,
}

/// Parse the leading `//@ args:` / `//@ stdin:` directives from a case's source.
///
/// - `//@ args: a b c` — whitespace-split into `argv[1..]`.
/// - `//@ stdin: ...` — the rest of the line, with `\n`/`\t`/`\0`/`\\`/`\xHH` un-escaped;
///   repeatable, each line gets a trailing newline.
/// - `//@ error: <substring>` — the program must be *rejected* by the front end with a
///   message containing `<substring>` (trimmed). See [`Directives::error`].
///
/// A case must not depend on `argv[0]`: the native binary's is its (temp) path, while the
/// oracle's is a fixed placeholder, so only `argv[1..]` is guaranteed to match.
pub fn parse_directives(src: &str) -> Directives {
    let mut args = Vec::new();
    let mut stdin = Vec::new();
    let mut error = None;
    for line in src.lines() {
        let t = line.trim_start();
        let Some(rest) = t.strip_prefix("//@") else {
            if t.is_empty() || t.starts_with("//") {
                continue; // blank / ordinary comment: keep scanning the header
            }
            break; // first real line ends the header
        };
        let rest = rest.trim_start();
        if let Some(a) = rest.strip_prefix("args:") {
            args.extend(a.split_whitespace().map(str::to_string));
        } else if let Some(s) = rest.strip_prefix("stdin:") {
            unescape_into(s.strip_prefix(' ').unwrap_or(s), &mut stdin);
            stdin.push(b'\n');
        } else if let Some(e) = rest.strip_prefix("error:") {
            error = Some(e.trim().to_string());
        }
    }
    Directives { args, stdin, error }
}

/// Un-escape a directive payload (`\n \t \r \0 \\ \xHH`) into raw bytes.
fn unescape_into(s: &str, out: &mut Vec<u8>) {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 1 < b.len() {
            match b[i + 1] {
                b'n' => out.push(b'\n'),
                b't' => out.push(b'\t'),
                b'r' => out.push(b'\r'),
                b'0' => out.push(0),
                b'\\' => out.push(b'\\'),
                b'x' if i + 3 < b.len() => {
                    let hex = std::str::from_utf8(&b[i + 2..i + 4]).unwrap_or("");
                    if let Ok(v) = u8::from_str_radix(hex, 16) {
                        out.push(v);
                        i += 4;
                        continue;
                    }
                    out.push(b'\\');
                }
                other => {
                    out.push(b'\\');
                    out.push(other);
                }
            }
            i += 2;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
}

// ---------------------------------------------------------------------------
// The integration entry point: one `.hc` case, generated as one `#[test]`.
// ---------------------------------------------------------------------------

/// Run a case as a **three-way agreement check**: the interpreter (the oracle), the
/// native binary, and a committed expected output (`<case>.out`) must all produce the same
/// bytes. The committed expected is the third anchor — without it a `native == interp`
/// check is blind to a bug both engines share (e.g. a `FmtFloat` change shifts both at
/// once); a frozen golden turns that into a caught regression.
///
/// - The interpreter output must equal the golden (catches interpreter/lowering drift).
/// - On a runnable host, the native output must equal the golden too (catches backend
///   bugs); off-host this self-skips and the matching CI leg covers it. Together the two
///   equalities give `native == interp == expected`.
/// - The emitted object is structurally validated on every host.
///
/// Regenerate the goldens with `SOLOMON_BLESS=1 cargo test --test integration` (writes the
/// interpreter output to each `<case>.out`); review the diff before committing.
pub fn run_case(rel: &str, src: &str) {
    let dir = case_dir(rel);
    let d = parse_directives(src);

    // Error-path case: assert the front end rejects this program (no golden, no native run).
    if let Some(expected) = &d.error {
        assert_rejected(rel, src, &dir, expected);
        return;
    }

    let program = parse_with(src, &dir, &[]).unwrap_or_else(|e| panic!("{rel}: parse failed: {e}"));
    let errs = check_program(&program);
    assert!(errs.is_empty(), "{rel}: semantic errors: {errs:?}");

    let mut argv = vec!["prog".to_string()];
    argv.extend(d.args.iter().cloned());
    let interp = run_to_bytes_with(&program, &argv, &d.stdin)
        .unwrap_or_else(|e| panic!("{rel}: interpreter error: {e}"));

    let golden = expected_path(rel);
    if bless() {
        std::fs::write(&golden, &interp)
            .unwrap_or_else(|e| panic!("{rel}: cannot write {}: {e}", golden.display()));
        return;
    }

    // The committed expected output — the independent third anchor.
    let expected = std::fs::read(&golden).unwrap_or_else(|e| {
        panic!(
            "{rel}: missing expected output {} ({e}); regenerate with SOLOMON_BLESS=1",
            golden.display()
        )
    });
    assert_eq!(
        interp,
        expected,
        "{rel}: interpreter output != committed expected ({})\n  interp:   {:?}\n  expected: {:?}",
        golden.display(),
        String::from_utf8_lossy(&interp),
        String::from_utf8_lossy(&expected),
    );

    // Structural — runs on every host, no toolchain, no execution.
    structural_macho(rel, &program);

    // End-to-end — only where the host target can build + run; self-skips otherwise.
    if let Some(native) = build_and_run_native(&program, &d.args, &d.stdin) {
        assert_eq!(
            native,
            expected,
            "{rel}: native output != committed expected ({})\n  native:   {:?}\n  expected: {:?}",
            golden.display(),
            String::from_utf8_lossy(&native),
            String::from_utf8_lossy(&expected),
        );
    }
}

/// Assert the front end **rejects** `src` with a message containing `expected` (the
/// `//@ error:` directive). Walks the three front-end stages in pipeline order — parse
/// (which folds in lex/preproc/mono), then sema, then layout — and matches `expected`
/// against the first error's `message`. An empty `expected` matches any error. Panics if
/// the program compiles cleanly (a stale "this should fail" case) or the message diverges.
fn assert_rejected(rel: &str, src: &str, dir: &Path, expected: &str) {
    let msg = match parse_with(src, dir, &[]) {
        Err(e) => e.message,
        Ok(program) => {
            if let Some(e) = check_program(&program).into_iter().next() {
                e.message
            } else if let Some(e) = hcc::layout::compute(&program).1.into_iter().next() {
                e.message
            } else {
                panic!(
                    "{rel}: expected a compile error containing {expected:?}, \
                     but the program compiled cleanly"
                );
            }
        }
    };
    assert!(
        msg.contains(expected),
        "{rel}: rejection message {msg:?} does not contain expected substring {expected:?}",
    );
}

/// Whether `SOLOMON_BLESS` is set: regenerate the `<case>.out` goldens instead of asserting.
fn bless() -> bool {
    std::env::var_os("SOLOMON_BLESS").is_some()
}

/// The committed expected-output path for a case (`<case>.out` beside its `.hc`). `rel` is
/// the case path relative to `tests/` (e.g. `"cases/pointers/deref.hc"`), as the `test_case!`
/// macro passes it.
fn expected_path(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join(rel)
        .with_extension("out")
}

/// The on-disk directory of a case (so any `#include "..."` resolves locally). `rel` is
/// relative to `tests/`.
fn case_dir(rel: &str) -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join(rel);
    p.pop();
    p
}

// ---------------------------------------------------------------------------
// Structural checks — byte-inspect the AArch64 Mach-O object (host-independent).
// ---------------------------------------------------------------------------

fn le_u32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
fn le_u64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

/// Emit the AArch64 Mach-O object for `program` and assert it is a well-formed container
/// with a non-empty `__text`. Needs no `cc`, so it exercises the emitter on any host.
pub fn structural_macho(rel: &str, program: &Program) {
    let obj = Arm64Darwin::new(std::env::temp_dir().join("solomon-it-structural-unused"))
        .object(program)
        .unwrap_or_else(|e| panic!("{rel}: arm64 object emission failed: {e}"));
    assert_eq!(le_u32(&obj, 0), 0xFEED_FACF, "{rel}: bad Mach-O magic");
    assert!(!macho_text(&obj).is_empty(), "{rel}: empty __text section");
}

/// The `__text` machine-code bytes of a Mach-O object.
pub fn macho_text(obj: &[u8]) -> &[u8] {
    assert_eq!(le_u32(obj, 0), 0xFEED_FACF, "bad Mach-O magic");
    let ncmds = le_u32(obj, 16);
    let mut off = 32;
    let mut seg = None;
    for _ in 0..ncmds {
        if le_u32(obj, off) == 0x19 {
            seg = Some(off);
            break;
        }
        off += le_u32(obj, off + 4) as usize;
    }
    let seg = seg.expect("no LC_SEGMENT_64");
    let nsects = le_u32(obj, seg + 64);
    let mut s = seg + 72;
    for _ in 0..nsects {
        if obj[s..s + 16].starts_with(b"__text\0") {
            let size = le_u64(obj, s + 40) as usize;
            let foff = le_u32(obj, s + 48) as usize;
            return &obj[foff..foff + size];
        }
        s += 80;
    }
    panic!("no __text section");
}

// ---------------------------------------------------------------------------
// Native build + run — the host target only. Returns None where unavailable.
// ---------------------------------------------------------------------------

/// `true` when `SOLOMON_SKIP_NATIVE` is set: drop to oracle + structural only (fast local
/// iteration). CI never sets it, so CI runs the full native lane.
pub fn skip_native() -> bool {
    std::env::var_os("SOLOMON_SKIP_NATIVE").is_some()
}

/// Whether `cc` is on PATH (needed to link the hosted Darwin target).
pub fn cc_available() -> bool {
    Command::new("cc")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Whether this host can build **and run** a native binary for its own target. Darwin
/// links via `cc`; the freestanding Linux ELF hosts run the emitted image directly.
/// Windows and other hosts are structural-only here.
pub fn native_host_available() -> bool {
    if skip_native() {
        return false;
    }
    if cfg!(all(target_arch = "aarch64", target_os = "macos")) {
        cc_available()
    } else {
        cfg!(any(
            all(target_arch = "x86_64", target_os = "linux"),
            all(target_arch = "aarch64", target_os = "linux"),
        ))
    }
}

/// The host target's code generator writing to `out`, or None if this host has no
/// runnable backend (Windows, Intel macOS, or `cc` missing on Darwin).
fn host_backend(out: &Path) -> Option<Box<dyn Codegen>> {
    #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
    {
        if !cc_available() {
            return None;
        }
        return Some(Box::new(Arm64Darwin::new(out)));
    }
    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    {
        return Some(Box::new(hcc::X64Linux::new(out)));
    }
    #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
    {
        return Some(Box::new(hcc::Arm64Linux::new(out)));
    }
    #[cfg(not(any(
        all(target_arch = "aarch64", target_os = "macos"),
        all(target_arch = "x86_64", target_os = "linux"),
        all(target_arch = "aarch64", target_os = "linux"),
    )))]
    {
        let _ = out;
        None
    }
}

/// Build `program` for the host target, run it with `args` (`argv[1..]`) and `stdin`, and
/// return its raw stdout. None when there is no runnable host backend (the case is then
/// covered structurally here and executed on the matching CI leg).
pub fn build_and_run_native(program: &Program, args: &[String], stdin: &[u8]) -> Option<Vec<u8>> {
    if skip_native() {
        return None;
    }
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let out = std::env::temp_dir().join(format!("solomon-it-{}-{id}", std::process::id()));
    let mut backend = host_backend(&out)?;
    backend
        .run(program)
        .unwrap_or_else(|e| panic!("native build failed: {e}"));
    let stdout = run_binary(&out, args, stdin);
    let _ = std::fs::remove_file(&out);
    Some(stdout)
}

/// Spawn `bin` with `args` and `stdin`, returning its stdout. Stdin is fed from a thread
/// so a program that writes a lot of stdout can't deadlock against an unread stdin pipe.
fn run_binary(bin: &Path, args: &[String], stdin: &[u8]) -> Vec<u8> {
    // Retry on ETXTBSY (os error 26): with many freshly-built native binaries exec'd in
    // parallel, one can still be open for writing in a sibling process, so the exec races and
    // fails transiently. A short bounded backoff clears it; this is a test-harness race, not a
    // codegen fault.
    let mut child = {
        let mut attempt = 0;
        loop {
            match Command::new(bin)
                .args(args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(c) => break c,
                Err(e) if e.raw_os_error() == Some(26) && attempt < 50 => {
                    attempt += 1;
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => panic!("could not spawn {}: {e}", bin.display()),
            }
        }
    };
    let mut sin = child.stdin.take().unwrap();
    let data = stdin.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = sin.write_all(&data);
    });
    let output = child
        .wait_with_output()
        .unwrap_or_else(|e| panic!("could not run {}: {e}", bin.display()));
    let _ = writer.join();
    output.stdout
}

// ---------------------------------------------------------------------------
// Comparison helpers — used by tests/comparison.rs (HolyC vs C).
// ---------------------------------------------------------------------------

/// Compile `src` for the host target to `out` (HolyC → native). Panics on build failure.
pub fn build_native_to(program: &Program, out: &Path) -> bool {
    let Some(mut backend) = host_backend(out) else {
        return false;
    };
    backend
        .run(program)
        .unwrap_or_else(|e| panic!("native build failed: {e}"));
    true
}

/// A fresh, process-unique temp path with the given `tag`.
pub fn temp_path(tag: &str) -> PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("solomon-{tag}-{}-{id}", std::process::id()))
}

/// Run `bin` once with no input and return its stdout (convenience for comparisons).
pub fn run_capture(bin: &Path) -> Vec<u8> {
    run_binary(bin, &[], &[])
}

/// Wall-clock time to run `bin` `iters` times back to back.
pub fn time_runs(bin: &Path, iters: u32) -> Duration {
    let start = Instant::now();
    for _ in 0..iters {
        let status = Command::new(bin)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap_or_else(|e| panic!("could not run {}: {e}", bin.display()));
        assert!(status.success(), "{} exited nonzero", bin.display());
    }
    start.elapsed()
}
