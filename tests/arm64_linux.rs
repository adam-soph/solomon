//! Conformance for the freestanding `aarch64-unknown-linux` backend: compile every
//! example to a self-contained static ELF (no libc, no linker) and run it — natively
//! on a linux/aarch64 host, otherwise in one `docker run --platform linux/arm64`
//! container (which runs AArch64 **natively** under Docker Desktop on Apple silicon —
//! no qemu) — asserting its stdout is byte-for-byte the interpreter's. Self-skips
//! when neither runner is available.

use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

use solomon::arm64::Arm64Linux;
use solomon::codegen::Codegen;
use solomon::interp::run_to_string;
use solomon::parser::{parse, parse_with};
use solomon::sema::check_program;

mod common;

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_dir() -> std::path::PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("solomon-arm64fs-{}-{id}", std::process::id()))
}

/// Run each freestanding ELF in `dir` and capture its stdout — directly on a
/// linux/aarch64 host, otherwise in one docker container (outputs split on a `0x1F`
/// marker printed after each). `None` to skip when neither path works.
fn run_stdouts(dir: &std::path::Path, names: &[String]) -> Option<Vec<String>> {
    if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        return names
            .iter()
            .map(|n| {
                Command::new(dir.join(n))
                    .output()
                    .ok()
                    .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            })
            .collect();
    }
    let script = names
        .iter()
        .map(|n| format!("/c/{n}; printf '\\037'"))
        .collect::<Vec<_>>()
        .join("\n");
    let out = Command::new("docker")
        .args([
            "run",
            "--platform",
            "linux/arm64",
            "--rm",
            "-v",
            &format!("{}:/c:ro", dir.display()),
            "alpine",
            "sh",
            "-c",
            &script,
        ])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let parts: Vec<String> = text.split('\u{1f}').map(str::to_string).collect();
    (parts.len() > names.len()).then(|| parts[..names.len()].to_vec())
}

#[test]
fn freestanding_matches_the_interpreter_for_every_example() {
    let dir = temp_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let mut names = Vec::new();
    let mut expected = Vec::new();
    for (file, src) in common::EXAMPLES {
        let name = file.trim_end_matches(".hc").to_string();
        let program = parse(src).unwrap_or_else(|e| panic!("{name}: parse failed: {e}"));
        assert!(check_program(&program).is_empty(), "{name}: sema errors");
        let want = run_to_string(&program).unwrap_or_else(|e| panic!("{name}: interp error: {e}"));
        Arm64Linux::new(dir.join(&name))
            .run(&program)
            .unwrap_or_else(|e| panic!("{name}: freestanding build failed: {e}"));
        names.push(name);
        expected.push(want);
    }
    let got = run_stdouts(&dir, &names);
    let _ = std::fs::remove_dir_all(&dir);
    let Some(got) = got else {
        eprintln!(
            "skipping aarch64 freestanding conformance: needs a linux/aarch64 host or docker"
        );
        return;
    };
    for (name, (out, want)) in names.iter().zip(got.iter().zip(&expected)) {
        assert_eq!(
            out, want,
            "freestanding native != interp stdout for example {name}"
        );
    }
}

#[test]
fn extreme_field_width_and_precision_do_not_overflow() {
    // Pathological width/precision is clamped at the shared `fmt` layer (width
    // ≤1024, precision ≤512) so the hand-emitted fixed scratch buffers in the
    // freestanding formatters never overflow. Pre-clamp these segfaulted; they
    // must now run and match the interpreter byte-for-byte.
    let cases: &[&str] = &[
        r#"U0 Main(){ "%2000d\n", 42; } Main;"#,
        r#"U0 Main(){ "%.800f\n", 3.14; } Main;"#,
        r#"U0 Main(){ "%.100d\n", 7; } Main;"#,
        r#"U0 Main(){ "[%2000s]\n", "tail"; } Main;"#,
        r#"U0 Main(){ "%.700e\n", 1.5; } Main;"#,
    ];
    let dir = temp_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let mut names = Vec::new();
    let mut expected = Vec::new();
    for (idx, src) in cases.iter().enumerate() {
        let program = parse(src).unwrap_or_else(|e| panic!("parse failed for `{src}`: {e}"));
        assert!(
            check_program(&program).is_empty(),
            "sema errors for `{src}`"
        );
        let want = run_to_string(&program).unwrap_or_else(|e| panic!("interp error: {e}"));
        let name = format!("w{idx}");
        Arm64Linux::new(dir.join(&name))
            .run(&program)
            .unwrap_or_else(|e| panic!("freestanding build failed for `{src}`: {e}"));
        names.push(name);
        expected.push(want);
    }
    let got = run_stdouts(&dir, &names);
    let _ = std::fs::remove_dir_all(&dir);
    let Some(got) = got else {
        eprintln!("skipping aarch64 width-clamp conformance: needs a linux/aarch64 host or docker");
        return;
    };
    for ((src, want), out) in cases.iter().zip(&expected).zip(&got) {
        assert_eq!(
            out, want,
            "freestanding native != interp stdout for `{src}`"
        );
    }
}

#[test]
fn stdlib_math_matches_the_interpreter() {
    // The HolyC standard library (`#include <math.hc>`) compiles freestanding and
    // prints exactly what the interpreter does — exercising angle includes through
    // the native pipeline and the F64 algebraic builtins (`Floor`/`Ceil`/`Round`).
    let src = r#"
        #include <math.hc>
        U0 Main() {
          "%.6f %.6f %.6f\n", Exp(1.0), Ln(E), Pow(2.0, 10.0);
          "%.6f %.6f %.6f\n", Sin(PI / 2.0), Cos(0.0), Tan(PI / 4.0);
          "%.1f %.1f %.1f %.1f\n", Round(2.5), Round(-2.5), Round(0.5), Round(-3.5);
          "%.1f %.1f %.1f %.1f\n", Floor(2.7), Floor(-2.3), Ceil(2.1), Ceil(-2.9);
          "%d %d %d\n", Gcd(48, 36), Factorial(6), IMax(3, 9);
        }
        Main;
    "#;
    let lib = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lib");
    let program = parse_with(src, std::path::Path::new("."), &[lib])
        .unwrap_or_else(|e| panic!("parse failed: {e}"));
    assert!(check_program(&program).is_empty(), "sema errors");
    let want = run_to_string(&program).unwrap_or_else(|e| panic!("interp error: {e}"));

    let dir = temp_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let name = "stdmath".to_string();
    Arm64Linux::new(dir.join(&name))
        .run(&program)
        .unwrap_or_else(|e| panic!("freestanding build failed: {e}"));
    let got = run_stdouts(&dir, &[name]);
    let _ = std::fs::remove_dir_all(&dir);
    let Some(got) = got else {
        eprintln!("skipping aarch64 stdlib conformance: needs a linux/aarch64 host or docker");
        return;
    };
    assert_eq!(
        got[0], want,
        "freestanding native != interp stdout for the math stdlib"
    );
}
