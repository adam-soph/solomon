//! Standard-library tests for the `lib/*.hc` HolyC modules. Each module is pulled in
//! via an angle include (`#include <math.hh>`) and run through **both** the interpreter
//! (the oracle) and the host-native binary, with the result compared against an
//! **independent expected value** — usually a Rust oracle (`format!("{:e}", …)` for the
//! float formatter, `str::parse` for the parsers) or a hand-known value. So every test is
//! a three-way agreement check: `native == interp == expected`.
//!
//! That independent expected is what the `native == interp` integration suite cannot
//! provide on its own: the printf/`FmtFloat`/`StrToF64` code is pure HolyC that *both*
//! engines run, so they'd agree on a shared bug. The Rust oracle here is the only check
//! that catches a *current* such bug, on both the interpreter and the native path.
//!
//! The native half self-skips off a runnable host (and under `HCC_SKIP_NATIVE`); the
//! matching CI leg covers it there.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use hcc::backend::Codegen;
use hcc::oracle::run_to_bytes_with;
use hcc::parser::parse_with;
use hcc::sema::check_program;
use hcc::{Arm64Darwin, Program};

/// Parse `src` with the repository's `lib/` on the angle-include search path, type-check,
/// and run it through the interpreter **and** (on a runnable host) the native backend,
/// asserting the two agree byte-for-byte before returning the output. The base directory
/// is `lib/` itself, so a test may exercise private helpers.
///
/// The caller then asserts the returned string against an independent expected value,
/// completing the `native == interp == expected` triple.
fn run_with_stdlib(src: &str) -> String {
    run_with_stdlib_input(src, &[])
}

/// Like [`run_with_stdlib`], but with `input` as the program's standard input (fd 0),
/// for exercising the `FGetC`/`FGetS`/`GetLine`/`ReadLine` family.
fn run_with_stdlib_input(src: &str, input: &[u8]) -> String {
    let lib = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lib");
    let program = parse_with(src, &lib, std::slice::from_ref(&lib))
        .unwrap_or_else(|e| panic!("parse failed: {e}"));
    let errs = check_program(&program);
    assert!(errs.is_empty(), "semantic errors: {errs:?}");
    let interp =
        run_to_bytes_with(&program, &[], input).unwrap_or_else(|e| panic!("runtime error: {e}"));
    // Hold the native backend to the same bytes (the test then checks these against the
    // independent expected, so all three agree).
    if let Some(native) = build_and_run_native(&program, &[], input) {
        assert_eq!(
            native,
            interp,
            "stdlib: native backend != interpreter\n  interp: {:?}\n  native: {:?}",
            String::from_utf8_lossy(&interp),
            String::from_utf8_lossy(&native),
        );
    }
    String::from_utf8_lossy(&interp).into_owned()
}

/// Run `src` through the interpreter **only** (no native comparison), for the rare stdlib
/// feature whose native lowering is intentionally host-divergent. Currently just `MSize`
/// on hosted Darwin: `MAlloc` maps to libc `malloc`, which exposes no requested-size
/// header, so `MSize` returns 0 there (documented at `src/arm64/isel/prims.rs`). The
/// interpreter defines the semantics; native `MSize` parity holds on the freestanding
/// targets by construction (the bump allocator's size header).
fn run_interp_only(src: &str) -> String {
    let lib = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lib");
    let program = parse_with(src, &lib, std::slice::from_ref(&lib))
        .unwrap_or_else(|e| panic!("parse failed: {e}"));
    let errs = check_program(&program);
    assert!(errs.is_empty(), "semantic errors: {errs:?}");
    let bytes =
        run_to_bytes_with(&program, &[], &[]).unwrap_or_else(|e| panic!("runtime error: {e}"));
    String::from_utf8_lossy(&bytes).into_owned()
}

#[test]
fn sscan_parses_fields_and_bases() {
    let out = run_with_stdlib(include_str!("sscan.hc"));
    assert_eq!(
        out,
        "n=5 a=42 b=-7 f=3.14 w=hello c=X\n\
         h=255 o=61 i1=16\n\
         m=1 e=1500.0\n\
         k=2 keep=1 skip=3\n\
         r=-1\n"
    );
}

#[test]
fn fgets_reads_stdin_lines_and_is_eof_safe() {
    let src = include_str!("fgets.hc");
    assert_eq!(
        run_with_stdlib_input(src, b"alpha\nbeta\ngamma"),
        "[alpha]\n[beta]\n[gamma]\nlines=3\n"
    );
    assert_eq!(run_with_stdlib_input(src, b""), "lines=0\n");
}

#[test]
fn getchar_and_readline_read_stdin() {
    // GetChar streams bytes; ReadLine allocates a NL-stripped line per call.
    let chars = run_with_stdlib_input(include_str!("getchar.hc"), b"abcde");
    assert_eq!(chars, "bytes=5\n");

    let lines = run_with_stdlib_input(include_str!("readline.hc"), b"one\ntwo\nthree");
    assert_eq!(lines, "<one>\n<two>\n<three>\n");
}

#[test]
fn errno_strerror_table() {
    let out = run_with_stdlib(include_str!("errno.hc"));
    assert_eq!(
        out,
        "Success\n\
         No such file or directory\n\
         No such file or directory\n\
         Invalid argument\n\
         File name too long\n\
         Connection refused\n\
         Unknown error\n"
    );
}

#[test]
fn strnprint_bounds_and_returns_would_be_length() {
    let out = run_with_stdlib(include_str!("strnprint.hc"));
    assert_eq!(
        out,
        "[42-hello!] r=9\n\
         [42-h] r=9\n\
         [] r=3\n\
         [ZZZ] r=3\n"
    );
}

#[test]
fn if_type_selects_branch_per_instantiation() {
    // `if type` is the single-case `switch type`: the untaken branch is discarded before
    // sema, so a dead arm ill-typed for the chosen `T` is fine. Covers `is`/`is not` + else.
    let out = run_with_stdlib(include_str!("if_type.hc"));
    assert_eq!(out, "float int other\n");
}

#[test]
fn min_max_preserve_element_type_and_handle_nan() {
    let out = run_with_stdlib(include_str!("min_max.hc"));
    assert_eq!(out, "3 9\n1.50 2.50\n5.0 5.0 5.0\n");
}

#[test]
fn abs_preserves_element_type_and_ieee_semantics() {
    let out = run_with_stdlib(include_str!("abs.hc"));
    assert_eq!(out, "7 7\n3.50 3.50\n0.0\n1\n");
}

#[test]
fn strtoi64base_handles_bases_and_endptr() {
    let out = run_with_stdlib(include_str!("strtoi64base.hc"));
    assert_eq!(
        out,
        "255 255 493 511 -5\n\
         endptr=[rest]\n\
         fail v=0 ateq=1\n\
         edge 0 [xZ]\n\
         compat 123 7 0\n"
    );
}

#[test]
fn strtoul_and_strtof64_endptr() {
    let out = run_with_stdlib(include_str!("strtoul_strtof64.hc"));
    assert_eq!(
        out,
        "18446744073709551615 18446744073709551615\n\
         uend=[zzz]\n\
         3.142\n\
         fend=[xyz]\n\
         -250.0\n\
         fail 0.0 ateq=1\n"
    );
}

#[test]
fn limits_and_float_constants() {
    let out = run_with_stdlib(include_str!("limits.hc"));
    assert_eq!(
        out,
        "-128 127 255 | -32768 32767 65535\n\
         -2147483648 2147483647 4294967295\n\
         -9223372036854775808 9223372036854775807 18446744073709551615\n\
         7fefffffffffffff 10000000000000 3cb0000000000000 1\n\
         7fefffffffffffff 3cb0000000000000\n"
    );
}

#[test]
fn strsep_preserves_empty_fields() {
    let out = run_with_stdlib(include_str!("strsep.hc"));
    assert_eq!(out, "[a][][b][]\n[][x]\nname=value null=1\n");
}

#[test]
fn string_h_workhorses() {
    let out = run_with_stdlib(include_str!("string_workhorses.hc"));
    assert_eq!(
        out,
        "abcd\n, world\n0 -1 0\ndup\ntrunc\na.bb.ccc.\none.two.\nkey=\n"
    );
}

#[test]
fn math_classification_and_integer_rounds() {
    let out = run_with_stdlib(include_str!("math_classification.hc"));
    assert_eq!(
        out,
        "3 -3 2 3 -1\n\
         2 4 -2 3\n\
         1 0 0 1\n\
         1 0 0 0 0\n\
         0 1 2 3 4\n"
    );
}

#[test]
fn time_difftime_localtime_and_cpu_clock() {
    let out = run_with_stdlib(include_str!("time_difftime.hc"));
    assert_eq!(out, "750.0\n2023-11-14 14:13:20\n1 1 1\n1000000\n");
}

#[test]
fn strftime_formats_dates() {
    let out = run_with_stdlib(include_str!("strftime.hc"));
    assert_eq!(
        out,
        "2023-11-14 22:13:20\n\
         Tue Tuesday Nov November\n\
         10:13 PM j=318 w=2 u=2\n\
         2023-11-14 22:13:20 22:13 11/14/23 23 %\n\
         Tue Nov 14 22:13:20 2023\n\
         trunc=0 ok=5\n\
         Thu 1970-01-01\n"
    );
}

#[test]
fn cheap_cleanups_fmax_div_strnlen_puts_errno() {
    let out = run_with_stdlib(include_str!("cheap_cleanups.hc"));
    assert_eq!(
        out,
        "2.5 1.5 5.0\n\
         -3 -1\n\
         3 2\n\
         Hi\n\
         line\n\
         Software caused connection abort|Operation canceled\n"
    );
}

#[test]
fn math_transcendentals_are_accurate() {
    let out = run_with_stdlib(include_str!("math_transcendentals.hc"));
    assert_eq!(
        out,
        "2.71828 1.00000 1024.00000\n\
         1.00000 1.00000 1.00000\n\
         0.00000 -1.00000\n"
    );
}

#[test]
fn math_rounding_and_integer_helpers() {
    let out = run_with_stdlib(include_str!("math_rounding.hc"));
    assert_eq!(
        out,
        "3.0 -3.0 1.0 -4.0\n\
         2.0 -3.0 3.0 -2.0\n\
         1024.0\n\
         12 720 3 9\n"
    );
}

#[test]
fn math_extended_functions() {
    let out = run_with_stdlib(include_str!("math_extended.hc"));
    assert_eq!(
        out,
        "2.0 3.0 3.0 -2.0\n\
         3.0000 3.0000 1024.0\n\
         0.785398 0.523599 1.047198\n\
         0.785398 2.356194\n\
         5.0 1.0\n"
    );
}

#[test]
fn math_go_style_functions() {
    // The Go-`math`-style additions: IEEE bits/classification, exponent ops, and the
    // extra elementary functions. Values match a reference `strtod`/libm. The native
    // backends are held to this same output by the conformance suites.
    let out = run_with_stdlib(include_str!("math_go_style.hc"));
    assert_eq!(
        out,
        "3ff0000000000000 2\n\
         1 1 1 1\n\
         -3.0 3.0\n\
         3 3.0\n\
         0.7500 4 12.0\n\
         3.00 0.75 3.0\n\
         3.000000 0.000001 0.000001\n\
         0.881374 1.316958 0.549306\n\
         -0.700000 10.000000 1000.0\n\
         0.479426 0.877583\n"
    );
}

#[test]
fn math_erf_and_gamma() {
    // The error-function and gamma family. Values match libm/scipy to ~10 decimals.
    // The implementations are Taylor + continued-fraction erf, Winitzki+Newton
    // inverses, and a Lanczos g=7 gamma. The native backends are held to this exact
    // output by the conformance suites.
    let out = run_with_stdlib(include_str!("math_erf_gamma.hc"));
    assert_eq!(
        out,
        "0.5204998778 0.8427007929 0.9953222650\n\
         0.004677734981 2.2090497e-05\n\
         0.4769362762 1.1630871537 0.4769362762\n\
         1.7724538509 24.0000000000 -3.5449077018\n\
         3.1780538303 1 0.5723649429 1 1.2655121235 -1\n"
    );
}

#[test]
fn math_bessel() {
    // J0/J1/Jn and Y0/Y1/Yn, spanning the small-x series and the large-x asymptotic.
    // The x=20 column crosses the threshold between them. Values match standard tables
    // to 10 decimals. The Wronskian J1·Y0 − J0·Y1 = 2/(πx) holds to ~1e-12 across the
    // range.
    let out = run_with_stdlib(include_str!("math_bessel.hc"));
    assert_eq!(
        out,
        "0.7651976866 -0.1775967713 0.1670246643\n\
         0.4400505857 -0.3275791376 0.0668331242\n\
         0.0882569642 -0.3085176252 0.0626405968\n\
         -0.7812128213 0.1478631434 -0.1655116144\n\
         0.4860912606 -0.2340615282 0.0000002515\n\
         -0.1604003935 0.1354030477 0.0265421593\n"
    );
}

#[test]
fn rand_seed_controls_the_stream() {
    // `RandU64` (in `<stdlib.hh>`): the generator is deterministic. `SeedRand` makes the stream
    // reproducible — the same seed gives the same value — and seed-dependent: a
    // different seed gives a different value.
    let out = run_with_stdlib(include_str!("rand_seed.hc"));
    assert_eq!(out, "1 1\n");
}

#[test]
fn strtof64_parses_correctly_rounded() {
    // The pure-HolyC `atof`: a correctly-rounded bignum parser. Covers the Clinger
    // fast path, the exact bignum slow path (long significands, large/small exponents,
    // the smallest normal double), the `%g`-printed round-trip, atof-style prefix
    // stopping, and over/underflow. Each value is the IEEE double nearest the decimal,
    // bit-identical to a reference `strtod`. That equivalence was verified separately
    // against Python over a 438-input random battery.
    let out = run_with_stdlib(include_str!("strtof64.hc"));
    assert_eq!(
        out,
        "0.10000000000000001 0.20000000000000001 0.29999999999999999\n\
         1e+30 1.2345678901234568e+17\n\
         2.2250738585072014e-308\n\
         1.7976931348623157e+308 6.0220000000000003e+23\n\
         3.140 -250.000 6.000\n\
         0 inf 0 -0\n"
    );
}

#[test]
fn holyc_float_f_formatter_matches_rust() {
    // The pure-HolyC `%f` formatter (`lib/stdio.hc`) must match Rust's `{:.P}`
    // byte-for-byte. It is the portable replacement for the hand-emitted bignum.
    // Coverage includes round-half-to-even ties, subnormals, the 2^53 boundary,
    // extremes, and -0.0. Each value is reconstructed from its exact bits via
    // `Float64frombits`, so the lexer's float parsing can't perturb the input.
    let values: &[f64] = &[
        0.0,
        -0.0,
        0.5,
        1.5,
        2.5,
        0.125,
        2.675,
        0.05,
        0.15,
        0.25,
        0.35,
        0.45,
        1.0,
        10.0,
        100.0,
        0.1,
        0.0001,
        0.00001,
        1000000.0,
        9999999.0,
        123456.789,
        std::f64::consts::PI,
        std::f64::consts::E,
        1.0 / 3.0,
        9007199254740991.0,
        9007199254740992.0,
        9007199254740993.0,
        1e300,
        1e-300,
        1e16,
        1e17,
        f64::MAX,
        f64::MIN_POSITIVE,
        f64::from_bits(1),
        -2.675,
        -123456.789,
        -1e300,
        -f64::from_bits(1),
    ];
    let precs: &[usize] = &[0, 1, 2, 3, 6, 10, 15, 17, 20];

    let mut src =
        String::from("#include <stdio.hh>\n#include <math.hh>\nU0 Main(){\nU8 b[2048];\n");
    let mut expected = String::new();
    for &v in values {
        for &p in precs {
            src.push_str(&format!(
                "FmtFloat(b, Float64frombits(0x{:016X}), 'f', 0, 0, {p}); \"%s\\n\", b;\n",
                v.to_bits()
            ));
            expected.push_str(&format!("{:.*}\n", p, v));
        }
    }
    // A few high precisions on a handful of values to exercise the large bignum.
    for &v in &[std::f64::consts::PI, 0.1, f64::from_bits(1), 1e300] {
        for &p in &[50usize, 120] {
            src.push_str(&format!(
                "FmtFloat(b, Float64frombits(0x{:016X}), 'f', 0, 0, {p}); \"%s\\n\", b;\n",
                v.to_bits()
            ));
            expected.push_str(&format!("{:.*}\n", p, v));
        }
    }
    src.push_str("}\nMain;\n");

    let out = run_with_stdlib(&src);
    if out != expected {
        // Find the first differing line for a readable failure.
        for (i, (a, b)) in out.lines().zip(expected.lines()).enumerate() {
            assert_eq!(a, b, "line {i} differs (got vs want)");
        }
        assert_eq!(out, expected, "output length differs");
    }
}

/// Render `mag` (non-negative) in C `%e`/`%E` form: a single leading digit, a
/// `precision`-digit fraction, then `e`/`E`, a sign, and an exponent of at least two
/// digits. Rust's correctly-rounded `{:e}` does the mantissa; this only restyles the
/// exponent to match libc. It is the **independent Rust oracle** the HolyC `%e`/`%g`
/// formatter is checked against — kept local to the test crate now that the
/// interpreter renders through the HolyC `FmtFloat` body rather than any Rust formatter.
fn render_exp(mag: f64, precision: usize, upper: bool) -> String {
    let s = format!("{:.*e}", precision, mag);
    let (mant, exp) = s.split_once('e').unwrap_or((s.as_str(), "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let e = if upper { 'E' } else { 'e' };
    let sign = if exp < 0 { '-' } else { '+' };
    format!("{mant}{e}{sign}{:02}", exp.unsigned_abs())
}

/// Render `mag` (non-negative) in C `%g`/`%G` form: `precision` significant digits,
/// choosing `%e` or `%f` by the post-rounding exponent and trimming trailing zeros
/// unless `alt` (the `#` flag) is set. The companion oracle to [`render_exp`].
fn render_g(mag: f64, precision: usize, upper: bool, alt: bool) -> String {
    let p = precision.max(1);
    // Format as %e at p-1 fractional digits to learn the rounded exponent X.
    let es = format!("{:.*e}", p - 1, mag);
    let (mant, exp) = es.split_once('e').unwrap_or((es.as_str(), "0"));
    let x: i32 = exp.parse().unwrap_or(0);
    let mut body = if x >= -4 && (x as i64) < p as i64 {
        // %f style with precision p-1-X.
        let fp = (p as i32 - 1 - x).max(0) as usize;
        format!("{:.*}", fp, mag)
    } else {
        let e = if upper { 'E' } else { 'e' };
        let sign = if x < 0 { '-' } else { '+' };
        format!("{mant}{e}{sign}{:02}", x.unsigned_abs())
    };
    if !alt {
        // Trim trailing zeros (and a bare `.`) from the mantissa, not the exponent.
        let (m, e) = match body.find(['e', 'E']) {
            Some(i) => (body[..i].to_string(), body[i..].to_string()),
            None => (body.clone(), String::new()),
        };
        if m.contains('.') {
            body = format!("{}{e}", m.trim_end_matches('0').trim_end_matches('.'));
        }
    }
    body
}

#[test]
fn holyc_float_e_g_formatter_matches_rust() {
    // The pure-HolyC `%e`/`%g` formatters (`lib/stdio.hc`) must match the local Rust
    // oracles `render_exp`/`render_g` byte-for-byte. Those renderers are Rust's
    // correctly-rounded `{:.Pe}` restyled to libc. Coverage includes `%g`'s
    // fixed/scientific choice, trailing-zero trim, and the `#` (alt) flag.
    let sign = |v: f64| if v.is_sign_negative() { "-" } else { "" };
    // The subnormal / extreme-exponent cases (×5^~1074, extracting ~750 digits) are
    // slow in the tree-walking interp, so they're kept few here. The bignum's
    // subnormal path is already covered cheaply by the `%f` test.
    let values: &[f64] = &[
        0.0,
        -0.0,
        1.5,
        1234.5,
        9.9999996,
        9.6,
        0.0001,
        0.00001,
        1000000.0,
        9999999.0,
        1234567.0,
        0.1,
        2.675,
        123456.789,
        std::f64::consts::PI,
        1.0 / 3.0,
        9007199254740993.0,
        1e16,
        1e17,
        1e300,
        f64::MAX,
        f64::from_bits(1),
        -2.5,
        -1234567.0,
        -1e300,
    ];
    let precs: &[usize] = &[0, 1, 3, 6, 17];

    let mut src =
        String::from("#include <stdio.hh>\n#include <math.hh>\nU0 Main(){\nU8 b[2048];\n");
    let mut expected = String::new();
    let emit = |src: &mut String, expected: &mut String, call: String, want: String| {
        src.push_str(&call);
        expected.push_str(&want);
        expected.push('\n');
    };
    for &v in values {
        let bits = v.to_bits();
        for &p in precs {
            // %e and %E
            emit(
                &mut src,
                &mut expected,
                format!(
                    "FmtFloat(b, Float64frombits(0x{bits:016X}), 'e', 0, 0, {p}); \"%s\\n\", b;\n"
                ),
                format!("{}{}", sign(v), render_exp(v.abs(), p, false)),
            );
            emit(
                &mut src,
                &mut expected,
                format!(
                    "FmtFloat(b, Float64frombits(0x{bits:016X}), 'E', 0, 0, {p}); \"%s\\n\", b;\n"
                ),
                format!("{}{}", sign(v), render_exp(v.abs(), p, true)),
            );
            // %g, %G, and %#G (alt keeps trailing zeros)
            emit(
                &mut src,
                &mut expected,
                format!(
                    "FmtFloat(b, Float64frombits(0x{bits:016X}), 'g', 0, 0, {p}); \"%s\\n\", b;\n"
                ),
                format!("{}{}", sign(v), render_g(v.abs(), p, false, false)),
            );
            emit(
                &mut src,
                &mut expected,
                format!(
                    "FmtFloat(b, Float64frombits(0x{bits:016X}), 'G', 64, 0, {p}); \"%s\\n\", b;\n"
                ),
                format!("{}{}", sign(v), render_g(v.abs(), p, true, true)),
            );
        }
    }
    src.push_str("}\nMain;\n");

    let out = run_with_stdlib(&src);
    if out != expected {
        for (i, (a, b)) in out.lines().zip(expected.lines()).enumerate() {
            assert_eq!(a, b, "line {i} differs (got vs want)");
        }
        assert_eq!(out, expected, "output length differs");
    }
}

#[test]
fn holyc_float_field_matches_interp() {
    // `FmtFloat` assembles the COMPLETE field: sign, width, and the `- 0 + space #`
    // flags. The `"%f", v` print form lowers to the same HolyC `Print`/`FmtFloat` path,
    // so this checks the printf wiring against a direct `FmtFloat` call: each case prints
    // the intrinsic and `FmtFloat` on adjacent lines and asserts the pair matches.
    // (spec, conv char, packed flag bits, width, precision)
    let cases: &[(&str, char, i64, usize, usize)] = &[
        ("%8.2f", 'f', 0, 8, 2),
        ("%-8.2f", 'f', 4, 8, 2),
        ("%08.2f", 'f', 8, 8, 2),
        ("%+.2f", 'f', 16, 0, 2),
        ("% .2f", 'f', 32, 0, 2),
        ("%012.4f", 'f', 8, 12, 4),
        ("%+08.1f", 'f', 24, 8, 1),
        ("%12.3e", 'e', 0, 12, 3),
        ("%-12.3e", 'e', 4, 12, 3),
        ("%015.2e", 'e', 8, 15, 2),
        ("%.3E", 'E', 0, 0, 3),
        ("%10.4g", 'g', 0, 10, 4),
        ("%+g", 'g', 16, 0, 6),
        ("%#g", 'g', 64, 0, 6),
        ("%#.3g", 'g', 64, 0, 3),
        ("%G", 'G', 0, 0, 6),
        ("%-12g", 'g', 4, 12, 6),
    ];
    let values: &[f64] = &[
        3.14159, -3.14159, 0.0, -0.0, 2.5, 1234.5, 0.00012345, 9999999.0, 1e6, -0.001, 42.0, 0.5,
    ];
    let mut src =
        String::from("#include <stdio.hh>\n#include <math.hh>\nU0 Main(){\nU8 b[2048];\n");
    for &(spec, conv, flags, width, prec) in cases {
        for &v in values {
            let bits = v.to_bits();
            src.push_str(&format!("\"{spec}\\n\", Float64frombits(0x{bits:016X});\n"));
            src.push_str(&format!(
                "FmtFloat(b, Float64frombits(0x{bits:016X}), '{conv}', {flags}, {width}, {prec}); \"%s\\n\", b;\n"
            ));
        }
    }
    src.push_str("}\nMain;\n");

    let out = run_with_stdlib(&src);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len() % 2, 0, "expected adjacent line pairs");
    for pair in lines.chunks(2) {
        assert_eq!(
            pair[0], pair[1],
            "intrinsic vs FmtFloat field mismatch (oracle left, FmtFloat right)"
        );
    }
}

#[test]
fn remquo_matches_rust() {
    // `Remquo` (lib/math.hc) against an independent Rust oracle: the remainder is
    // x - y·round_ties_even(x/y) computed in f64 (identical operations, so identical
    // bits — compared via the raw bit pattern, not a decimal rendering), and the
    // quotient word is the low 3 bits of |round_ties_even(x/y)| carrying sign(x/y).
    let pairs: &[(f64, f64)] = &[
        (7.0, 2.0),
        (-7.0, 2.0),
        (7.0, -2.0),
        (-7.0, -2.0),
        (17.5, 4.0),
        (1.0, 8.0),
        (100.0, 3.0),
        (12.5, 2.5),
        (0.5, 1.5),
        (1e10, 7.0),
        (5.25, -0.75),
        (3.0, 3.0),
        (0.0, 5.0),
        (1e300, 7.0),
        (std::f64::consts::PI, std::f64::consts::E),
    ];
    let mut src =
        String::from("#include <stdio.hh>\n#include <math.hh>\nU0 Main(){\nI64 q;\nF64 r;\n");
    let mut expected = String::new();
    for &(x, y) in pairs {
        src.push_str(&format!(
            "r = Remquo(Float64frombits(0x{:016X}), Float64frombits(0x{:016X}), &q); \
             \"%016X %d\\n\", Float64bits(r), q;\n",
            x.to_bits(),
            y.to_bits()
        ));
        let qf = (x / y).round_ties_even();
        let r = x - y * qf;
        let mut quo = (qf.abs() % 8.0) as i64;
        if (x < 0.0) != (y < 0.0) {
            quo = -quo;
        }
        expected.push_str(&format!("{:016X} {}\n", r.to_bits(), quo));
    }
    src.push_str("}\nMain;\n");
    assert_eq!(run_with_stdlib(&src), expected);
}

#[test]
fn mktime_strptime_round_trip() {
    // FromUnix -> Strftime -> StrPTime -> MkTime must return to the same epoch second
    // (and the same normalized fields, `wday` included) for a sweep covering the epoch,
    // negatives (pre-1970), leap days, and far future. Also pins MkTime's field
    // normalization (carry out-of-range month/day/minute) against C mktime semantics.
    let epochs: &[i64] = &[
        0,
        1,
        -1,
        86399,
        -86400,
        951782399,    // 2000-02-28 23:59:59 (leap-century boundary)
        951782400,    // 2000-02-29
        1700000000,   // 2023-11-14
        4102444800,   // 2100-01-01 (non-leap century)
        -2208988800,  // 1900-01-01
        253402300799, // 9999-12-31 23:59:59
    ];
    let mut src = String::from("#include <time.hh>\nU0 Main(){\nU8 b[64];\nDateTime d, p;\n");
    let mut expected = String::new();
    for &s in epochs {
        src.push_str(&format!(
            "d = FromUnix({s});\n\
             Strftime(b, 64, \"%Y-%m-%d %H:%M:%S\", d);\n\
             p.year = 0; p.month = 1; p.day = 1; p.hour = 0; p.min = 0; p.sec = 0; p.wday = 0;\n\
             \"%d %d\\n\", StrPTime(b, \"%Y-%m-%d %H:%M:%S\", &p) != NULL && MkTime(&p) == {s},\n\
             p.wday == d.wday;\n"
        ));
        expected.push_str("1 1\n");
    }
    src.push_str("}\nMain;\n");
    assert_eq!(run_with_stdlib(&src), expected);
}

#[test]
fn realloc_preserves_contents() {
    // ReAlloc keeps the first min(old, new) bytes across a grow and a shrink, and a
    // NULL argument behaves like MAlloc. Pointer identity is implementation-defined:
    // in-place on a bump allocator, moved on libc/interp. So only the bytes are
    // asserted.
    let out = run_with_stdlib(include_str!("realloc.hc"));
    assert_eq!(out, "abcdef\nabcd\n");
}

#[test]
fn msize_reports_the_requested_allocation_size() {
    // `MSize` returns the byte size MAlloc was asked for (0 for NULL), survives the
    // size header transparently, and tracks through a ReAlloc grow. Interp-only: hosted
    // Darwin maps `MAlloc` to libc `malloc` and cannot recover the requested size, so
    // native `MSize` returns 0 there (a documented limitation, not a parity bug).
    let out = run_interp_only(include_str!("msize.hc"));
    assert_eq!(out, "40 7 0\nok\n16 80\n");
}

#[test]
fn vec_object_grows_and_clones() {
    // The owning, growable generic `Vec<T>`: push across reallocations, at/ref/pop,
    // and a deep clone (independent buffer). Monomorphized over every kind of element:
    // I64, F64, a pointer, and a class value. The class case round-trips a whole value
    // through the byte heap buffer, both on store (`VecPush`) and load (`VecAt`).
    let out = run_with_stdlib(include_str!("vec.hc"));
    assert_eq!(
        out,
        "len=10 capok=1 at5=25\n\
         pop=81 pop=64 len=8\n\
         clone0=99 clen=9 vlen=8\n\
         f64 1.5 2.5\n\
         ptr a b\n\
         class 3 4\n"
    );
}

#[test]
fn time_calendar_math() {
    // FromUnix/FmtISO/ToUnix over fixed epochs. These are pure, so the output is
    // reproducible. Covers the epoch, a leap year, and a pre-1970 negative timestamp.
    let out = run_with_stdlib(include_str!("time_calendar.hc"));
    assert_eq!(
        out,
        "1970-01-01 00:00:00 w4 L0 r1\n\
         2024-06-01 00:00:00 w6 L1 r1\n\
         2001-09-09 01:46:40 w0 L0 r1\n\
         1969-12-31 00:00:00 w3 L0 r1\n"
    );
}

#[test]
fn generic_value_param_array_runs() {
    // `int N` value parameter as an array dimension: `T data[N]` becomes `I64 data[4]`.
    let out = run_with_stdlib(include_str!("generic_value_param.hc"));
    assert_eq!(out, "sum=60 size=32\n"); // 4*8 = 32 bytes; 0+10+20+30 = 60
}

#[test]
fn generic_type_switch_selects_and_discards() {
    // Each instantiation keeps only its arm. The `Pt` arm uses `v.x` (valid only when
    // T = Pt); for the I64/F64/U8* instantiations it is discarded before sema, so it
    // never errors. `Show("hi")` matches no arm and falls to `default`.
    let out = run_with_stdlib(include_str!("generic_type_switch.hc"));
    assert_eq!(out, "int 7\nflt 2.5\npt 9\nother\n");
}

// ===========================================================================
// Native build/run harness (formerly tests/common). Inlined so this test
// binary is self-contained: build a stdlib program for the host and run it, so
// the native backend can be held to the interpreter's bytes.
// ===========================================================================

/// Process-wide counter so concurrently-built temp binaries never collide on a path.
static COUNTER: AtomicU32 = AtomicU32::new(0);

/// `true` when `HCC_SKIP_NATIVE` is set: drop to the interpreter only (fast local
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
/// return its raw stdout. None when there is no runnable host backend (the matching CI leg
/// executes it there).
fn build_and_run_native(program: &Program, args: &[String], stdin: &[u8]) -> Option<Vec<u8>> {
    if skip_native() {
        return None;
    }
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let out = std::env::temp_dir().join(format!("hcc-stdlib-{}-{id}", std::process::id()));
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
    // Retry on ETXTBSY (os error 26): a freshly-built binary may still be open for writing in
    // a sibling process, so the exec races transiently. A short bounded backoff clears it.
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
