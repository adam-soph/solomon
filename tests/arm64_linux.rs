//! Conformance for the freestanding `aarch64-unknown-linux` backend. Compile every
//! example to a self-contained static ELF (no libc, no linker) and run it natively,
//! asserting its stdout is byte-for-byte the interpreter's. Execution runs only on a
//! linux/aarch64 host and self-skips elsewhere. So off a Mac, `cargo test` exercises
//! the AArch64 freestanding emitter through the build but not execution. (The shared
//! AArch64 emitter is executed on Darwin by `arm64_darwin`.)

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

/// Run each freestanding ELF in `dir` and capture its stdout, natively and only on a
/// linux/aarch64 host. The freestanding ELF runs directly, with no emulation. Returns
/// `None` to skip off a non-matching host; CI covers the Linux targets.
fn run_stdouts(dir: &std::path::Path, names: &[String]) -> Option<Vec<String>> {
    if !cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        return None;
    }
    names
        .iter()
        .map(|n| {
            Command::new(dir.join(n))
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        })
        .collect()
}

#[test]
fn freestanding_matches_the_interpreter_for_every_example() {
    let dir = temp_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let mut names = Vec::new();
    let mut expected = Vec::new();
    for (file, src) in common::EXAMPLES {
        let name = file.trim_end_matches(".hc").to_string();
        let program =
            common::parse_example(src).unwrap_or_else(|e| panic!("{name}: parse failed: {e}"));
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
        eprintln!("skipping aarch64 freestanding conformance: needs a linux/aarch64 host");
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
    // Pathological width/precision is clamped at the shared `fmt` layer (width ≤1024,
    // precision ≤512), so the hand-emitted fixed scratch buffers in the freestanding
    // formatters never overflow. These cases segfaulted before the clamp. They must
    // now run and match the interpreter byte-for-byte.
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
        eprintln!("skipping aarch64 width-clamp conformance: needs a linux/aarch64 host");
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
fn realloc_extends_the_last_block_in_place() {
    // The payoff of the `HeapExtend` builtin. Growing the heap's last allocation
    // extends it in place on the freestanding bump allocator: the pointer never moves,
    // so there's no copy and no leak. This differs from the libc/interp heaps. Also
    // checks the contents survive the grows.
    let src = r#"
        #include <string.hc>
        #include <stdlib.hc>
        U0 Main() {
          U8 *q = MAlloc(16);
          StrCpy(q, "keep");
          I64 moves = 0, i;
          for (i = 1; i <= 6; i++) {
            U8 *prev = q;
            q = ReAlloc(q, 16 * i, 16 * (i + 1));
            if (q != prev) moves++;
          }
          "%s moves=%d\n", q, moves;
        }
        Main;
    "#;
    let lib = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lib");
    let program = parse_with(src, std::path::Path::new("."), &[lib])
        .unwrap_or_else(|e| panic!("parse failed: {e}"));
    assert!(check_program(&program).is_empty(), "sema errors");
    let dir = temp_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let name = "realloc".to_string();
    Arm64Linux::new(dir.join(&name))
        .run(&program)
        .unwrap_or_else(|e| panic!("freestanding build failed: {e}"));
    let got = run_stdouts(&dir, &[name]);
    let _ = std::fs::remove_dir_all(&dir);
    let Some(got) = got else {
        eprintln!("skipping aarch64 ReAlloc in-place conformance: needs a linux/aarch64 host");
        return;
    };
    // In place every time → the bump pointer is never abandoned.
    assert_eq!(
        got[0], "keep moves=0\n",
        "freestanding ReAlloc did not extend in place"
    );
}

#[test]
fn dynamic_width_and_precision_match_the_interpreter() {
    // `*` width/precision (taken from arguments) in the freestanding formatter must
    // match the interpreter byte-for-byte, for ints, strings, and floats. This covers
    // a negative `*` width (left-justify) and a negative `*` precision (no precision).
    let cases: &[&str] = &[
        r#"U0 Main(){ "[%*d][%-*d][%*d]\n", 5, 42, 5, 42, -5, 42; } Main;"#,
        r#"U0 Main(){ "[%.*d][%*.*d]\n", 3, 7, 8, 4, 42; } Main;"#,
        r#"U0 Main(){ "[%.*f][%*.*f]\n", 2, 3.14159, 10, 3, 2.5; } Main;"#,
        r#"U0 Main(){ "[%.*e][%*g]\n", 3, 1234.5, 12, 1.5; } Main;"#,
        r#"U0 Main(){ "[%.*s][%*s][%-*s]\n", 3, "hello", 8, "hi", 8, "hi"; } Main;"#,
        r#"U0 Main(){ "[%.*d][%*c]\n", -1, 7, 4, 'x'; } Main;"#,
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
        let name = format!("star{idx}");
        Arm64Linux::new(dir.join(&name))
            .run(&program)
            .unwrap_or_else(|e| panic!("freestanding build failed for `{src}`: {e}"));
        names.push(name);
        expected.push(want);
    }
    let got = run_stdouts(&dir, &names);
    let _ = std::fs::remove_dir_all(&dir);
    let Some(got) = got else {
        eprintln!(
            "skipping aarch64 dynamic width/precision conformance: needs a linux/aarch64 host"
        );
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
    // prints exactly what the interpreter does. This exercises angle includes through
    // the native pipeline and the F64 algebraic builtins (`Floor`/`Ceil`/`Round`).
    let src = r#"
        #include <math.hc>
        U0 Main() {
          "%.6f %.6f %.6f\n", Exp(1.0), Ln(E), Pow(2.0, 10.0);
          "%.6f %.6f %.6f\n", Sin(PI / 2.0), Cos(0.0), Tan(PI / 4.0);
          "%.6f %.6f %.6f\n", Atan(1.0), Log10(1000.0), Hypot(3.0, 4.0);
          "%.6f %.6f %.6f\n", Sinh(1.0), Asin(0.5), Atan2(1.0, -1.0);
          "%.1f %.1f %.1f %.1f\n", Round(2.5), Round(-2.5), Round(0.5), Round(-3.5);
          "%.1f %.1f %.1f %.1f\n", Floor(2.7), Floor(-2.3), Ceil(2.1), Ceil(-2.9);
          "%d %d %d\n", Gcd(48, 36), Factorial(6), Max(3, 9);
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
        eprintln!("skipping aarch64 stdlib conformance: needs a linux/aarch64 host");
        return;
    };
    assert_eq!(
        got[0], want,
        "freestanding native != interp stdout for the math stdlib"
    );
}

#[test]
fn strtof64_matches_the_interpreter() {
    // The correctly-rounded `atof` (`#include <stdlib.hc>`, over its private `Bn`)
    // compiles and runs freestanding. Previously `StrToF64` lowered to a libc `_atof`
    // the static ELF couldn't resolve. Both paths (Clinger fast and exact bignum slow)
    // must print byte-for-byte what the interpreter does.
    let src = r#"
        #include <stdlib.hc>
        U0 Main() {
          "%.17g %.17g %.17g\n", StrToF64("0.1"), StrToF64("0.2"), StrToF64("0.3");
          "%.17g %.17g\n", StrToF64("1e30"), StrToF64("123456789012345678");
          "%.17g %.17g\n", StrToF64("2.2250738585072014e-308"), StrToF64("6.022e23");
          "%.3f %.3f %.3f\n", StrToF64("3.14"), StrToF64("-2.5e2"), StrToF64("  6.0x");
          "%g %g %g\n", StrToF64("xyz"), StrToF64("1e309"), StrToF64("1e-330");
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
    let name = "strtof64".to_string();
    Arm64Linux::new(dir.join(&name))
        .run(&program)
        .unwrap_or_else(|e| panic!("freestanding build failed: {e}"));
    let got = run_stdouts(&dir, &[name]);
    let _ = std::fs::remove_dir_all(&dir);
    let Some(got) = got else {
        eprintln!("skipping aarch64 StrToF64 conformance: needs a linux/aarch64 host");
        return;
    };
    assert_eq!(
        got[0], want,
        "freestanding native != interp stdout for StrToF64"
    );
}

#[test]
fn time_builtins_run_natively() {
    // Time is impure (non-reproducible), so assert properties of the native run rather
    // than byte-comparing to the interpreter: wall clock past 1970, and a monotonic
    // clock that doesn't go backwards across a Sleep.
    let src = r#"
    #include <time.hc>
    U0 Main() {
        I64 a = NanoNS();
        Sleep(2000000);
        I64 b = NanoNS();
        "%d %d\n", UnixNS() > 1000000000000000000, b >= a;
    } Main;"#;
    let program = parse(src).unwrap_or_else(|e| panic!("parse failed: {e}"));
    assert!(check_program(&program).is_empty(), "sema errors");
    let dir = temp_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let name = "timeprog".to_string();
    Arm64Linux::new(dir.join(&name))
        .run(&program)
        .unwrap_or_else(|e| panic!("freestanding build failed: {e}"));
    let got = run_stdouts(&dir, &[name]);
    let _ = std::fs::remove_dir_all(&dir);
    let Some(got) = got else {
        eprintln!("skipping aarch64 time conformance: needs a linux/aarch64 host");
        return;
    };
    assert_eq!(got[0], "1 1\n", "time builtin properties hold natively");
}

#[test]
fn variadic_functions_match_the_interpreter() {
    // The freestanding vararg ABI held byte-for-byte to the interpreter.
    let src = r#"
        I64 SumI(...) { I64 s=0,i=0; while(i<VargC){s+=VargV[i];i++;} return s; }
        F64 AvgF(...) { F64 s=0.0; I64 i=0; while(i<VargC){s+=*(F64*)&VargV[i];i++;} return s/VargC; }
        U0 Join(U8 *sep, ...) { I64 i=0; while(i<VargC){ if(i)"%s",sep; "%s",*(U8**)&VargV[i]; i++; } "\n"; }
        U0 Main() {
          "%d %d\n", SumI(10,20,30,40), SumI(7);
          "%.3f\n", AvgF(1.0,2.0,6.0);
          Join(" | ", "x", "y", "z");
        }
        Main;
    "#;
    // `parse_with` so the implicit `builtin.hc` prelude (NULL/TRUE/FALSE) is in scope.
    let program = parse_with(src, std::path::Path::new("."), &[])
        .unwrap_or_else(|e| panic!("parse failed: {e}"));
    assert!(check_program(&program).is_empty(), "sema errors");
    let want = run_to_string(&program).unwrap_or_else(|e| panic!("interp error: {e}"));
    let dir = temp_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let name = "varargs".to_string();
    Arm64Linux::new(dir.join(&name))
        .run(&program)
        .unwrap_or_else(|e| panic!("freestanding build failed: {e}"));
    let got = run_stdouts(&dir, &[name]);
    let _ = std::fs::remove_dir_all(&dir);
    let Some(got) = got else {
        eprintln!("skipping aarch64 varargs conformance: needs a linux/aarch64 host");
        return;
    };
    assert_eq!(got[0], want, "freestanding native != interp for varargs");
}

#[test]
fn time_calendar_math_matches_the_interpreter() {
    // The pure calendar math in lib/time.hc held byte-for-byte to the interpreter.
    let src = r#"
        #include <time.hc>
        U0 Show(I64 s) {
          U8 b[32]; DateTime dt = FromUnix(s);
          "%s w%d r%d\n", FmtISO(b, dt), dt.wday, ToUnix(dt) == s;
        }
        U0 Main() { Show(0); Show(1717200000); Show(1000000000); Show(-86400); }
        Main;
    "#;
    let lib = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lib");
    let program = parse_with(src, std::path::Path::new("."), &[lib])
        .unwrap_or_else(|e| panic!("parse failed: {e}"));
    assert!(check_program(&program).is_empty(), "sema errors");
    let want = run_to_string(&program).unwrap_or_else(|e| panic!("interp error: {e}"));
    let dir = temp_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let name = "timecal".to_string();
    Arm64Linux::new(dir.join(&name))
        .run(&program)
        .unwrap_or_else(|e| panic!("freestanding build failed: {e}"));
    let got = run_stdouts(&dir, &[name]);
    let _ = std::fs::remove_dir_all(&dir);
    let Some(got) = got else {
        eprintln!("skipping aarch64 time.hc conformance: needs a linux/aarch64 host");
        return;
    };
    assert_eq!(
        got[0], want,
        "freestanding native != interp for lib/time.hc"
    );
}
