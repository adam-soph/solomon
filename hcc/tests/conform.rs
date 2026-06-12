//! Integration tests: one per `tests/conform/**/*.hc` program.
//!
//! There is one `#[test]` per category directory (e.g. `conform_floats`); each calls the
//! `run_dir` helper, which walks that subtree **at runtime** and runs every `.hc` file it
//! finds through `run_case`. This replaces the former `test_case!` proc-macro, which
//! globbed the tree at compile time and emitted one `#[test]` per file (and lived in the
//! now-removed `tools/macros` crate). Because discovery is at runtime, adding or removing a
//! `.hc` is picked up with no rebuild dance — just re-run the tests.
//!
//! Each case compiles its program on the host target and asserts byte-for-byte stdout
//! parity with the IR-interpreter oracle plus a committed `.out` golden (a host-independent
//! structural check runs on every host). `run_case` and the harness live below; run a
//! single category with e.g. `cargo test --test conform conform_floats`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use hcc::backend::Codegen;
use hcc::oracle::run_to_bytes_with;
use hcc::parser::parse_with;
use hcc::sema::check_program;
use hcc::{Arm64Darwin, Program};

// One `#[test]` per category directory under `tests/conform/`. Each runs every `.hc` case
// in its subtree via `run_dir`; categories parallelise as independent tests.
#[test]
fn conform_args() {
    run_dir("conform/args");
}
#[test]
fn conform_arithmetic() {
    run_dir("conform/arithmetic");
}
#[test]
fn conform_arrays() {
    run_dir("conform/arrays");
}
#[test]
fn conform_classes() {
    run_dir("conform/classes");
}
#[test]
fn conform_control_flow() {
    run_dir("conform/control_flow");
}
#[test]
fn conform_ctype() {
    run_dir("conform/ctype");
}
#[test]
fn conform_errors() {
    run_dir("conform/errors");
}
#[test]
fn conform_exceptions() {
    run_dir("conform/exceptions");
}
#[test]
fn conform_floats() {
    run_dir("conform/floats");
}
#[test]
fn conform_functions() {
    run_dir("conform/functions");
}
#[test]
fn conform_generics() {
    run_dir("conform/generics");
}
#[test]
fn conform_globals() {
    run_dir("conform/globals");
}
#[test]
fn conform_inheritance() {
    run_dir("conform/inheritance");
}
#[test]
fn conform_initializers() {
    run_dir("conform/initializers");
}
#[test]
fn conform_int_width() {
    run_dir("conform/int_width");
}
#[test]
fn conform_math() {
    run_dir("conform/math");
}
#[test]
fn conform_pointers() {
    run_dir("conform/pointers");
}
#[test]
fn conform_ported_examples() {
    run_dir("conform/ported_examples");
}
#[test]
fn conform_preprocessor() {
    run_dir("conform/preprocessor");
}
#[test]
fn conform_relational() {
    run_dir("conform/relational");
}
#[test]
fn conform_stdio_printf() {
    run_dir("conform/stdio_printf");
}
#[test]
fn conform_stdlib() {
    run_dir("conform/stdlib");
}
#[test]
fn conform_stdlib_string() {
    run_dir("conform/stdlib_string");
}
#[test]
fn conform_strings() {
    run_dir("conform/strings");
}
#[test]
fn conform_switch() {
    run_dir("conform/switch");
}
#[test]
fn conform_time() {
    run_dir("conform/time");
}
#[test]
fn conform_tuples() {
    run_dir("conform/tuples");
}
#[test]
fn conform_typedefs_fnptr() {
    run_dir("conform/typedefs_fnptr");
}
#[test]
fn conform_unions() {
    run_dir("conform/unions");
}

/// Run every `.hc` case under `tests/<category>` through [`run_case`], reporting all the
/// failures in that category together rather than stopping at the first. `category` is the
/// path relative to `tests/`, e.g. `"conform/floats"`.
fn run_dir(category: &str) {
    let tests = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests");
    let dir = tests.join(category);
    let mut files = Vec::new();
    collect_hc(&dir, &mut files);
    files.sort();
    assert!(
        !files.is_empty(),
        "{category}: no .hc cases found under {}",
        dir.display()
    );
    let mut failures = Vec::new();
    for path in &files {
        let rel = path
            .strip_prefix(&tests)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let src = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("{}: cannot read case: {e}", path.display()));
        // Run each case under `catch_unwind` so one failing case doesn't hide the rest of
        // its category; the default panic hook still prints each failure's message (which
        // names the case), and the names are collected for the summary below.
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_case(&rel, &src))).is_err()
        {
            failures.push(rel);
        }
    }
    assert!(
        failures.is_empty(),
        "{} case(s) failed in {category}:\n  {}",
        failures.len(),
        failures.join("\n  ")
    );
}

/// Recursively collect every `*.hc` file under `dir` into `out`.
fn collect_hc(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()));
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            collect_hc(&p, out);
        } else if p.extension().is_some_and(|x| x == "hc") {
            out.push(p);
        }
    }
}

// ===========================================================================
// Test harness (formerly tests/common). Inlined so this test binary is
// self-contained: the per-`.hc` three-way agreement check, the directive
// parser, the host build/run, and the structural Mach-O check.
// ===========================================================================

/// Process-wide counter so concurrently-running cases never collide on a temp path.
static COUNTER: AtomicU32 = AtomicU32::new(0);

// ---- directives: the leading `//@ args:` / `//@ stdin:` / `//@ error:` header ----

/// The program arguments (`argv[1..]`, *not* `argv[0]`) and stdin bytes a case requests,
/// plus the optional error-path directive.
struct Directives {
    args: Vec<String>,
    stdin: Vec<u8>,
    /// `//@ error: <substring>` — flips the case into a *rejection* check: the front end
    /// must fail to compile the program with a message containing this substring (no golden,
    /// no native run). The empty string matches any error.
    error: Option<String>,
}

/// Parse the leading `//@ args:` / `//@ stdin:` / `//@ error:` directives from a case's
/// source. Only leading `//@ ...` lines are read; scanning stops at the first other line.
fn parse_directives(src: &str) -> Directives {
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

// ---- run_case: one `.hc`, run as one `#[test]` ----

/// Run a case as a **three-way agreement check**: the interpreter (the oracle), the native
/// binary, and a committed expected output (`<case>.out`) must all produce the same bytes.
/// The committed expected is the third anchor — without it a `native == interp` check is
/// blind to a bug both engines share (e.g. a `FmtFloat` change shifts both at once); a
/// frozen golden turns that into a caught regression.
///
/// - The interpreter output must equal the golden (catches interpreter/lowering drift).
/// - On a runnable host, the native output must equal the golden too; off-host this
///   self-skips and the matching CI leg covers it. Together: `native == interp == expected`.
/// - The emitted object is structurally validated on every host.
///
/// Regenerate the goldens with `HCC_BLESS=1 cargo test --test cases`; review the diff first.
fn run_case(rel: &str, src: &str) {
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
            "{rel}: missing expected output {} ({e}); regenerate with HCC_BLESS=1",
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
/// (lex/preproc/mono folded in), then sema, then layout — and matches `expected` against
/// the first error's `message`. An empty `expected` matches any error.
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

/// Whether `HCC_BLESS` is set: regenerate the `<case>.out` goldens instead of asserting.
fn bless() -> bool {
    std::env::var_os("HCC_BLESS").is_some()
}

/// The committed expected-output path for a case (`<case>.out` beside its `.hc`). `rel` is
/// the case path relative to `tests/` (e.g. `"cases/pointers/deref.hc"`).
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

// ---- native build + run (host target only) ----

/// `true` when `HCC_SKIP_NATIVE` is set: drop to oracle + structural only (fast local
/// iteration). CI never sets it, so CI runs the full native lane.
fn skip_native() -> bool {
    std::env::var_os("HCC_SKIP_NATIVE").is_some()
}

/// Whether `cc` is on PATH (needed to link the hosted Darwin target).
#[allow(dead_code)] // only consulted in the Darwin `host_backend` branch
fn cc_available() -> bool {
    Command::new("cc")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// The host target's code generator writing to `out`, or None if this host has no runnable
/// backend (Windows, Intel macOS, or `cc` missing on Darwin).
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
fn build_and_run_native(program: &Program, args: &[String], stdin: &[u8]) -> Option<Vec<u8>> {
    if skip_native() {
        return None;
    }
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let out = std::env::temp_dir().join(format!("hcc-it-{}-{id}", std::process::id()));
    let mut backend = host_backend(&out)?;
    backend
        .run(program)
        .unwrap_or_else(|e| panic!("native build failed: {e}"));
    let stdout = run_binary(&out, args, stdin);
    let _ = std::fs::remove_file(&out);
    Some(stdout)
}

/// Spawn `bin` with `args` and `stdin`, returning its stdout. Stdin is fed from a thread so
/// a program that writes a lot of stdout can't deadlock against an unread stdin pipe.
fn run_binary(bin: &Path, args: &[String], stdin: &[u8]) -> Vec<u8> {
    // Retry on ETXTBSY (os error 26): with many freshly-built native binaries exec'd in
    // parallel, one can still be open for writing in a sibling process, so the exec races and
    // fails transiently. A short bounded backoff clears it; this is a test-harness race.
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

// ---- structural: byte-check the AArch64 Mach-O (host-independent) ----

/// Emit the AArch64 Mach-O object for `program` and assert it is a well-formed container
/// with a non-empty `__text`. Needs no `cc`, so it exercises the emitter on any host.
fn structural_macho(rel: &str, program: &Program) {
    let obj = Arm64Darwin::new(std::env::temp_dir().join("hcc-it-structural-unused"))
        .object(program)
        .unwrap_or_else(|e| panic!("{rel}: arm64 object emission failed: {e}"));
    assert_eq!(le_u32(&obj, 0), 0xFEED_FACF, "{rel}: bad Mach-O magic");
    assert!(!macho_text(&obj).is_empty(), "{rel}: empty __text section");
}

/// The `__text` machine-code bytes of a Mach-O object.
fn macho_text(obj: &[u8]) -> &[u8] {
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

fn le_u32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
fn le_u64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}
