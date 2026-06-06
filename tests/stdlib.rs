//! Standard-library tests: the `lib/*.hc` HolyC modules, resolved via angle
//! includes (`#include <math.hc>`) and run through the interpreter (the oracle).
//! Host-independent — the native backends are held to this output byte-for-byte by
//! the `stdlib_math_matches_the_interpreter` conformance tests.

use solomon::interp::run_to_string;
use solomon::parser::parse_with;
use solomon::sema::check_program;

/// Parse `src` with the repository's `lib/` on the angle-include search path, then
/// type-check and run it, returning captured output. The base directory is `lib/`
/// itself, so a test may exercise private `_`-prefixed helpers (`_`-file/dir privacy
/// is keyed on the file's path — a program rooted in `lib/` is inside the subtree the
/// stdlib's private modules are visible from).
fn run_with_stdlib(src: &str) -> String {
    let lib = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lib");
    let program = parse_with(src, &lib, std::slice::from_ref(&lib))
        .unwrap_or_else(|e| panic!("parse failed: {e}"));
    let errs = check_program(&program);
    assert!(errs.is_empty(), "semantic errors: {errs:?}");
    run_to_string(&program).unwrap_or_else(|e| panic!("runtime error: {e}"))
}

#[test]
fn math_transcendentals_are_accurate() {
    let out = run_with_stdlib(
        r#"
        #include <math.hc>
        U0 Main() {
          "%.5f %.5f %.5f\n", Exp(1.0), Ln(E), Pow(2.0, 10.0);
          "%.5f %.5f %.5f\n", Sin(PI / 2.0), Cos(0.0), Tan(PI / 4.0);
          "%.5f %.5f\n", Sin(0.0), Cos(PI);
        }
        Main;
    "#,
    );
    assert_eq!(
        out,
        "2.71828 1.00000 1024.00000\n\
         1.00000 1.00000 1.00000\n\
         0.00000 -1.00000\n"
    );
}

#[test]
fn math_rounding_and_integer_helpers() {
    let out = run_with_stdlib(
        r#"
        #include <math.hc>
        U0 Main() {
          "%.1f %.1f %.1f %.1f\n", Round(2.5), Round(-2.5), Round(0.5), Round(-3.5);
          "%.1f %.1f %.1f %.1f\n", Floor(2.7), Floor(-2.3), Ceil(2.1), Ceil(-2.9);
          "%.1f\n", PowI(2.0, 10);
          "%d %d %d %d\n", Gcd(48, 36), Factorial(6), IMin(3, 9), IMax(3, 9);
        }
        Main;
    "#,
    );
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
    let out = run_with_stdlib(
        r#"
        #include <math.hc>
        U0 Main() {
          "%.1f %.1f %.1f %.1f\n", Floor(2.7), Ceil(2.1), Round(2.5), Trunc(-2.9);
          "%.4f %.4f %.1f\n", Log10(1000.0), Log2(8.0), Exp2(10.0);
          "%.6f %.6f %.6f\n", Atan(1.0), Asin(0.5), Acos(0.5);
          "%.6f %.6f\n", Atan2(1.0, 1.0), Atan2(1.0, -1.0);
          "%.1f %.1f\n", Hypot(3.0, 4.0), Fmod(7.0, 3.0);
        }
        Main;
    "#,
    );
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
    // extra elementary functions. Values match a reference `strtod`/libm; the native
    // backends are held to this same output by the conformance suites.
    let out = run_with_stdlib(
        r#"
        #include <math.hc>
        U0 Main() {
          "%x %g\n", Float64bits(1.0), Float64frombits(4611686018427387904);
          "%d %d %d %d\n", IsNaN(NaN()), IsInf(Inf(1), 1), IsInf(Inf(-1), -1), Signbit(-0.0);
          "%.1f %.1f\n", Copysign(3.0, -1.0), Copysign(-3.0, 1.0);
          "%d %.1f\n", Ilogb(8.0), Logb(8.0);
          I64 e; F64 fr = Frexp(12.0, &e); "%.4f %d %.1f\n", fr, e, Ldexp(0.75, 4);
          F64 ip; F64 fp = Modf(3.75, &ip); "%.2f %.2f %.1f\n", ip, fp, Dim(5.0, 2.0);
          "%.6f %.6f %.6f\n", Cbrt(27.0), Expm1(1e-6), Log1p(1e-6);
          "%.6f %.6f %.6f\n", Asinh(1.0), Acosh(2.0), Atanh(0.5);
          "%.6f %.6f %.1f\n", Remainder(5.3, 2.0), FMA(2.0, 3.0, 4.0), Pow10(3);
          F64 s, c; Sincos(0.5, &s, &c); "%.6f %.6f\n", s, c;
        }
        Main;
    "#,
    );
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
    // The error-function and gamma family. Values match libm/scipy to ~10 decimals
    // (Taylor + continued-fraction erf, Winitzki+Newton inverses, Lanczos g=7 gamma);
    // the native backends are held to this exact output by the conformance suites.
    let out = run_with_stdlib(
        r#"
        #include <special.hc>
        U0 Main() {
          "%.10f %.10f %.10f\n", Erf(0.5), Erf(1.0), Erf(2.0);
          "%.10g %.10g\n", Erfc(2.0), Erfc(3.0);
          "%.10f %.10f %.10f\n", Erfinv(0.5), Erfinv(0.9), Erfcinv(0.5);
          "%.10f %.10f %.10f\n", Gamma(0.5), Gamma(5.0), Gamma(-0.5);
          I64 s1, s2, s3;
          F64 l1 = Lgamma(5.0, &s1), l2 = Lgamma(0.5, &s2), l3 = Lgamma(-0.5, &s3);
          "%.10f %d %.10f %d %.10f %d\n", l1, s1, l2, s2, l3, s3;
        }
        Main;
    "#,
    );
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
    // J0/J1/Jn and Y0/Y1/Yn, spanning the small-x series and the large-x asymptotic
    // (the x=20 column crosses the threshold). Values match standard tables to 10
    // decimals; the Wronskian J1·Y0 − J0·Y1 = 2/(πx) holds to ~1e-12 across the range.
    let out = run_with_stdlib(
        r#"
        #include <special.hc>
        U0 Main() {
          "%.10f %.10f %.10f\n", J0(1.0), J0(5.0), J0(20.0);
          "%.10f %.10f %.10f\n", J1(1.0), J1(5.0), J1(20.0);
          "%.10f %.10f %.10f\n", Y0(1.0), Y0(5.0), Y0(20.0);
          "%.10f %.10f %.10f\n", Y1(1.0), Y1(5.0), Y1(20.0);
          "%.10f %.10f %.10f\n", Jn(2, 3.0), Jn(5, 10.0), Jn(10, 2.0);
          "%.10f %.10f %.10f\n", Yn(2, 3.0), Yn(5, 10.0), Yn(3, 8.0);
        }
        Main;
    "#,
    );
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
    // `rand.hc`: the generator is deterministic, and `SeedRand` makes the stream
    // reproducible (same seed → same value) and seed-dependent (a different seed
    // gives a different value).
    let out = run_with_stdlib(
        r#"
        #include <rand.hc>
        U0 Main() {
          SeedRand(1); I64 a = RandU64();
          SeedRand(1); I64 b = RandU64();
          SeedRand(2); I64 c = RandU64();
          "%d %d\n", a == b, a != c;
        }
        Main;
    "#,
    );
    assert_eq!(out, "1 1\n");
}

#[test]
fn strtof64_parses_correctly_rounded() {
    // The pure-HolyC `atof` (bignum, correctly rounded). Covers the Clinger fast
    // path, the exact bignum slow path (long significands, large/small exponents,
    // the smallest normal double), the `%g`-printed round-trip, atof-style prefix
    // stopping, and over/underflow. Each value is the IEEE double nearest the
    // decimal — bit-identical to a reference `strtod` (verified separately against
    // Python over a 438-input random battery).
    let out = run_with_stdlib(
        r#"
        #include <strconv.hc>
        U0 Main() {
          "%.17g %.17g %.17g\n", StrToF64("0.1"), StrToF64("0.2"), StrToF64("0.3");
          "%.17g %.17g\n", StrToF64("1e30"), StrToF64("123456789012345678");
          "%.17g\n", StrToF64("2.2250738585072014e-308");   // smallest normal
          "%.17g %.17g\n", StrToF64("1.7976931348623157e308"), StrToF64("6.022e23");
          "%.3f %.3f %.3f\n", StrToF64("3.14"), StrToF64("-2.5e2"), StrToF64("  6.0x");
          "%g %g %g %g\n", StrToF64("xyz"), StrToF64("1e309"), StrToF64("1e-330"), StrToF64("-0.0");
        }
        Main;
    "#,
    );
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
    // The pure-HolyC `%f` formatter (`lib/fltfmt.hc`, the portable replacement for
    // the hand-emitted bignum) must be byte-for-byte Rust's `{:.P}` — including
    // round-half-to-even ties, subnormals, the 2^53 boundary, extremes, and -0.0.
    // Each value is reconstructed from its exact bits via `Float64frombits`, so the
    // lexer's float parsing can't perturb the input.
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

    let mut src = String::from("#include <_fltfmt.hc>\nU0 Main(){\nU8 b[2048];\n");
    let mut expected = String::new();
    for &v in values {
        for &p in precs {
            src.push_str(&format!(
                "_FmtFloat(b, Float64frombits(0x{:016X}), 'f', 0, 0, {p}); \"%s\\n\", b;\n",
                v.to_bits()
            ));
            expected.push_str(&format!("{:.*}\n", p, v));
        }
    }
    // A few high precisions on a handful of values to exercise the large bignum.
    for &v in &[std::f64::consts::PI, 0.1, f64::from_bits(1), 1e300] {
        for &p in &[50usize, 120] {
            src.push_str(&format!(
                "_FmtFloat(b, Float64frombits(0x{:016X}), 'f', 0, 0, {p}); \"%s\\n\", b;\n",
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

#[test]
fn holyc_float_e_g_formatter_matches_rust() {
    // The pure-HolyC `%e`/`%g` formatters (`lib/fltfmt.hc`) must match the
    // interpreter's renderers `fmt::render_exp`/`render_g` (which are Rust's
    // correctly-rounded `{:.Pe}` restyled to libc) byte-for-byte — including `%g`'s
    // fixed/scientific choice, trailing-zero trim, and the `#` (alt) flag.
    use solomon::fmt::{render_exp, render_g};
    let sign = |v: f64| if v.is_sign_negative() { "-" } else { "" };
    // The subnormal / extreme-exponent cases (×5^~1074, extracting ~750 digits) are
    // slow in the tree-walking interp, so they're kept few here; the bignum's
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

    let mut src = String::from("#include <_fltfmt.hc>\nU0 Main(){\nU8 b[2048];\n");
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
                    "_FmtFloat(b, Float64frombits(0x{bits:016X}), 'e', 0, 0, {p}); \"%s\\n\", b;\n"
                ),
                format!("{}{}", sign(v), render_exp(v.abs(), p, false)),
            );
            emit(
                &mut src,
                &mut expected,
                format!(
                    "_FmtFloat(b, Float64frombits(0x{bits:016X}), 'E', 0, 0, {p}); \"%s\\n\", b;\n"
                ),
                format!("{}{}", sign(v), render_exp(v.abs(), p, true)),
            );
            // %g, %G, and %#G (alt keeps trailing zeros)
            emit(
                &mut src,
                &mut expected,
                format!(
                    "_FmtFloat(b, Float64frombits(0x{bits:016X}), 'g', 0, 0, {p}); \"%s\\n\", b;\n"
                ),
                format!("{}{}", sign(v), render_g(v.abs(), p, false, false)),
            );
            emit(
                &mut src,
                &mut expected,
                format!(
                    "_FmtFloat(b, Float64frombits(0x{bits:016X}), 'G', 64, 0, {p}); \"%s\\n\", b;\n"
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
    // `_FmtFloat` assembles the COMPLETE field (sign + width + the `- 0 + space #`
    // flags); it must equal the interpreter's own `%`-rendering (its magnitude
    // renderer wrapped by `render_int`) byte-for-byte. Each case prints the intrinsic
    // and `_FmtFloat` on adjacent lines and asserts the pair matches.
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
    let mut src = String::from("#include <_fltfmt.hc>\nU0 Main(){\nU8 b[2048];\n");
    for &(spec, conv, flags, width, prec) in cases {
        for &v in values {
            let bits = v.to_bits();
            src.push_str(&format!("\"{spec}\\n\", Float64frombits(0x{bits:016X});\n"));
            src.push_str(&format!(
                "_FmtFloat(b, Float64frombits(0x{bits:016X}), '{conv}', {flags}, {width}, {prec}); \"%s\\n\", b;\n"
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
            "intrinsic vs _FmtFloat field mismatch (oracle left, _FmtFloat right)"
        );
    }
}

#[test]
fn realloc_preserves_contents() {
    // ReAlloc keeps the first min(old, new) bytes across a grow and a shrink, and
    // NULL behaves like MAlloc. (Pointer identity is impl-defined — in-place on a
    // bump allocator, moved on libc/interp — so only the bytes are asserted.)
    let out = run_with_stdlib(
        r#"
        #include <cstr.hc>
        #include <mem.hc>
        U0 Main() {
          U8 *p = ReAlloc(NULL, 0, 8);   // == MAlloc(8)
          StrCpy(p, "abcdef");
          p = ReAlloc(p, 8, 64);          // grow
          "%s\n", p;
          p = ReAlloc(p, 64, 4);          // shrink (keeps first 4)
          "%c%c%c%c\n", p[0], p[1], p[2], p[3];
        }
        Main;
    "#,
    );
    assert_eq!(out, "abcdef\nabcd\n");
}

#[test]
fn msize_reports_the_requested_allocation_size() {
    // `MSize` returns the byte size MAlloc was asked for (0 for NULL), survives the
    // size header transparently, and tracks through a ReAlloc grow.
    let out = run_with_stdlib(
        r#"
        #include <cstr.hc>
        #include <mem.hc>
        U0 Main() {
          U8 *p = MAlloc(40); "%d ", MSize(p);
          U8 *q = MAlloc(7);  "%d ", MSize(q);
          "%d\n", MSize(NULL);
          StrCpy(p, "ok"); "%s\n", p;       // contents unaffected by the header
          U8 *r = MAlloc(16); "%d ", MSize(r);
          r = ReAlloc(r, 16, 80); "%d\n", MSize(r);
        }
        Main;
    "#,
    );
    assert_eq!(out, "40 7 0\nok\n16 80\n");
}

#[test]
fn vec_object_grows_and_clones() {
    // The owning, growable generic `Vec<T>`: push across reallocations, at/ref/pop,
    // deep clone (independent buffer), monomorphized over every kind of element — I64,
    // F64, a pointer, and a class value (the class case round-trips a whole value
    // through the byte heap buffer, both on store `VecPush` and load `VecAt`).
    let out = run_with_stdlib(
        r#"
        #include <vec.hc>
        class Pt { I64 x; I64 y; }
        U0 Main() {
          Vec<I64> v; VecInit(&v);
          I64 i;
          for (i = 0; i < 10; i++) VecPush(&v, i * i);
          "len=%d capok=%d at5=%d\n", VecLen(&v), v.cap >= v.len, VecAt(&v, 5);
          "pop=%d pop=%d len=%d\n", VecPop(&v), VecPop(&v), VecLen(&v);

          VecSet(&v, 0, 99);
          Vec<I64> c; VecClone(&c, &v);
          VecPush(&c, 7);
          "clone0=%d clen=%d vlen=%d\n", VecAt(&c, 0), VecLen(&c), VecLen(&v);

          Vec<F64> f; VecInit(&f);            // F64 elements
          VecPush(&f, 1.5);
          VecPush(&f, 2.5);
          "f64 %.1f %.1f\n", VecAt(&f, 0), VecAt(&f, 1);

          Vec<U8 *> s; VecInit(&s);           // pointer elements
          VecPush(&s, "a");
          VecPush(&s, "b");
          Vec<U8 *> sc; VecClone(&sc, &s);    // clone keeps the pointers valid
          "ptr %s %s\n", VecAt(&s, 0), VecAt(&sc, 1);

          Vec<Pt> p; VecInit(&p);             // class values
          Pt e; e.x = 1; e.y = 2; VecPush(&p, e);
          e.x = 3; e.y = 4; VecPush(&p, e);
          Pt g = VecAt(&p, 1);                // load a whole class by value
          "class %d %d\n", g.x, g.y;

          VecFree(&v); VecFree(&c); VecFree(&f); VecFree(&s); VecFree(&sc); VecFree(&p);
        }
        Main;
    "#,
    );
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
    // FromUnix/FmtISO/ToUnix over fixed epochs (pure → reproducible). Covers the
    // epoch, a leap year, and a pre-1970 negative timestamp.
    let out = run_with_stdlib(
        r#"
        #include <time.hc>
        U0 Show(I64 s) {
          U8 b[32]; DateTime dt = FromUnix(s);
          "%s w%d L%d r%d\n", FmtISO(b, dt), dt.wday, IsLeap(dt.year), ToUnix(dt) == s;
        }
        U0 Main() { Show(0); Show(1717200000); Show(1000000000); Show(-86400); }
        Main;
    "#,
    );
    assert_eq!(
        out,
        "1970-01-01 00:00:00 w4 L0 r1\n\
         2024-06-01 00:00:00 w6 L1 r1\n\
         2001-09-09 01:46:40 w0 L0 r1\n\
         1969-12-31 00:00:00 w3 L0 r1\n"
    );
}
