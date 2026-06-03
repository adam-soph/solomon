//! Standard-library tests: the `lib/*.hc` HolyC modules, resolved via angle
//! includes (`#include <math.hc>`) and run through the interpreter (the oracle).
//! Host-independent — the native backends are held to this output byte-for-byte by
//! the `stdlib_math_matches_the_interpreter` conformance tests.

use solomon::interp::run_to_string;
use solomon::parser::parse_with;
use solomon::sema::check_program;

/// Parse `src` with the repository's `lib/` on the angle-include search path, then
/// type-check and run it, returning captured output.
fn run_with_stdlib(src: &str) -> String {
    let lib = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lib");
    let program = parse_with(src, std::path::Path::new("."), &[lib])
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
         0 inf 0 0\n"
    );
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
    // The owning, growable generic `Vec`: emplace-push across reallocations, at/pop,
    // deep clone (independent buffer), and the *same* `Vec` type over every kind of
    // element — I64, F64, a pointer, and a class value — chosen by `VecInit(esize)`.
    let out = run_with_stdlib(
        r#"
        #include <vec.hc>
        class Pt { I64 x; I64 y; }
        U0 Main() {
          Vec v; VecInit(&v, sizeof(I64));
          I64 i;
          for (i = 0; i < 10; i++) *(I64 *)VecPush(&v) = i * i;
          "len=%d capok=%d at5=%d\n", v.len, v.cap >= v.len, *(I64 *)VecAt(&v, 5);
          "pop=%d pop=%d len=%d\n", *(I64 *)VecPop(&v), *(I64 *)VecPop(&v), v.len;

          *(I64 *)VecAt(&v, 0) = 99;
          Vec c; VecClone(&c, &v);
          *(I64 *)VecPush(&c) = 7;
          "clone0=%d clen=%d vlen=%d\n", *(I64 *)VecAt(&c, 0), c.len, v.len;

          Vec f; VecInit(&f, sizeof(F64));   // F64 elements
          *(F64 *)VecPush(&f) = 1.5;
          *(F64 *)VecPush(&f) = 2.5;
          "f64 %.1f %.1f\n", *(F64 *)VecAt(&f, 0), *(F64 *)VecAt(&f, 1);

          Vec s; VecInit(&s, sizeof(U8 *));  // pointer elements
          *(U8 **)VecPush(&s) = "a";
          *(U8 **)VecPush(&s) = "b";
          Vec sc; VecClone(&sc, &s);         // clone keeps the pointers valid
          "ptr %s %s\n", *(U8 **)VecAt(&s, 0), *(U8 **)VecAt(&sc, 1);

          Vec p; VecInit(&p, sizeof(Pt));    // class values
          Pt *e = VecPush(&p); e->x = 1; e->y = 2;
          e = VecPush(&p); e->x = 3; e->y = 4;
          Pt *g = VecAt(&p, 1);
          "class %d %d\n", g->x, g->y;

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
