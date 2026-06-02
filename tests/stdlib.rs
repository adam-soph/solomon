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
