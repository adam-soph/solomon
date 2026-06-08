//! Standard-library tests for the `lib/*.hc` HolyC modules. Each module is pulled in
//! via an angle include (`#include <math.hc>`) and run through the interpreter, the
//! conformance oracle. These tests are host-independent. The native backends are held
//! to this output byte-for-byte by the `stdlib_math_matches_the_interpreter`
//! conformance tests.

use solomon::interp::{run_to_string, run_to_string_with_input};
use solomon::parser::parse_with;
use solomon::sema::check_program;

/// Parse `src` with the repository's `lib/` on the angle-include search path, then
/// type-check and run it, returning captured output. The base directory is `lib/`
/// itself, so a test may exercise private `_`-prefixed helpers. Privacy for a `_`-file
/// or `_`-dir is keyed on the file's path, and a program rooted in `lib/` sits inside
/// the subtree from which the stdlib's private modules are visible.
fn run_with_stdlib(src: &str) -> String {
    let lib = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lib");
    let program = parse_with(src, &lib, std::slice::from_ref(&lib))
        .unwrap_or_else(|e| panic!("parse failed: {e}"));
    let errs = check_program(&program);
    assert!(errs.is_empty(), "semantic errors: {errs:?}");
    run_to_string(&program).unwrap_or_else(|e| panic!("runtime error: {e}"))
}

/// Like [`run_with_stdlib`], but with `input` as the program's standard input (fd 0),
/// for exercising the `FGetC`/`FGetS`/`GetLine`/`ReadLine` family through the oracle.
fn run_with_stdlib_input(src: &str, input: &[u8]) -> String {
    let lib = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lib");
    let program = parse_with(src, &lib, std::slice::from_ref(&lib))
        .unwrap_or_else(|e| panic!("parse failed: {e}"));
    let errs = check_program(&program);
    assert!(errs.is_empty(), "semantic errors: {errs:?}");
    run_to_string_with_input(&program, input).unwrap_or_else(|e| panic!("runtime error: {e}"))
}

#[test]
fn sscan_parses_fields_and_bases() {
    let out = run_with_stdlib(
        r#"
        #include <stdio.hc>
        U0 Main() {
          I64 a, b; F64 f; U8 w[16]; U8 c;
          I64 n = SScan("  42 -7 3.14 hello X", "%d %d %f %s %c", &a, &b, &f, w, &c);
          "n=%d a=%d b=%d f=%.2f w=%s c=%c\n", n, a, b, f, w, c;
          I64 h, o, i1;                       // hex, octal, %i auto-base
          SScan("0xFF 075 0x10", "%x %o %i", &h, &o, &i1);
          "h=%d o=%d i1=%d\n", h, o, i1;
          F64 e; I64 z;                        // scientific float; %d then fails -> count 1
          I64 m = SScan("1.5e3 zzz", "%f %d", &e, &z);
          "m=%d e=%.1f\n", m, e;
          I64 keep, skip;                      // '*' suppresses assignment
          I64 k = SScan("1 2 3", "%d %*d %d", &keep, &skip);
          "k=%d keep=%d skip=%d\n", k, keep, skip;
          I64 only;                            // EOF before any match -> -1
          "r=%d\n", SScan("", "%d", &only);
        }
        Main;
    "#,
    );
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
    let src = r#"
        #include <stdio.hc>
        U0 Main() {
          U8 line[32];
          I64 n = 0;
          while (FGetS(line, 32, STDIN)) {
            I64 len = StrLen(line);
            if (len > 0 && line[len - 1] == '\n') line[len - 1] = 0;
            "[%s]\n", line;
            n++;
          }
          "lines=%d\n", n;
        }
        Main;
    "#;
    assert_eq!(
        run_with_stdlib_input(src, b"alpha\nbeta\ngamma"),
        "[alpha]\n[beta]\n[gamma]\nlines=3\n"
    );
    assert_eq!(run_with_stdlib_input(src, b""), "lines=0\n");
}

#[test]
fn getchar_and_readline_read_stdin() {
    // GetChar streams bytes; ReadLine allocates a NL-stripped line per call.
    let chars = run_with_stdlib_input(
        r#"
        #include <stdio.hc>
        U0 Main() {
          I64 c, n = 0;
          while ((c = GetChar()) >= 0) n++;
          "bytes=%d\n", n;
        }
        Main;
    "#,
        b"abcde",
    );
    assert_eq!(chars, "bytes=5\n");

    let lines = run_with_stdlib_input(
        r#"
        #include <stdio.hc>
        U0 Main() {
          U8 *l;
          while ((l = ReadLine(STDIN))) { "<%s>\n", l; Free(l); }
        }
        Main;
    "#,
        b"one\ntwo\nthree",
    );
    assert_eq!(lines, "<one>\n<two>\n<three>\n");
}

#[test]
fn errno_strerror_table() {
    let out = run_with_stdlib(
        r#"
        #include <errno.hc>
        U0 Main() {
          "%s\n", StrError(0);
          "%s\n", StrError(ENOENT);
          "%s\n", StrError(-ENOENT);    // a negative -errno is accepted too
          "%s\n", StrError(EINVAL);
          "%s\n", StrError(ENAMETOOLONG);
          "%s\n", StrError(ECONNREFUSED);
          "%s\n", StrError(99999);      // unknown -> generic
        }
        Main;
    "#,
    );
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
    let out = run_with_stdlib(
        r#"
        #include <stdio.hc>
        U0 Main() {
          U8 buf[32];
          I64 r;
          r = StrNPrint(buf, 32, "%d-%s!", 42, "hello");  // fits: "42-hello!"
          "[%s] r=%d\n", buf, r;
          r = StrNPrint(buf, 5, "%d-%s!", 42, "hello");    // truncate to cap-1 = 4
          "[%s] r=%d\n", buf, r;
          r = StrNPrint(buf, 1, "abc");                    // only the NUL fits
          "[%s] r=%d\n", buf, r;
          StrCpy(buf, "ZZZ");
          r = StrNPrint(buf, 0, "abc");                    // nothing written, still counts
          "[%s] r=%d\n", buf, r;
        }
        Main;
    "#,
    );
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
    let out = run_with_stdlib(
        r#"
        #include <stdio.hc>
        U8 *Kind<type T>(T x) {
          if type (T is F64) return "float";
          if type (T is not I64) return "other";
          else return "int";
        }
        U0 Main() { "%s %s %s\n", Kind(1.5), Kind(42), Kind("hi"); }
        Main;
    "#,
    );
    assert_eq!(out, "float int other\n");
}

#[test]
fn min_max_preserve_element_type_and_handle_nan() {
    let out = run_with_stdlib(
        r#"
        #include <math.hc>
        U0 Main() {
          "%d %d\n", Min(3, 9), Max(3, 9);          // integers stay I64
          "%.2f %.2f\n", Min(2.5, 1.5), Max(2.5, 1.5); // floats are F64, not truncated
          F64 nan = NaN();                              // fmin/fmax: NaN -> the other
          "%.1f %.1f %.1f\n", Max(5.0, nan), Max(nan, 5.0), Min(nan, 5.0);
        }
        Main;
    "#,
    );
    assert_eq!(out, "3 9\n1.50 2.50\n5.0 5.0 5.0\n");
}

#[test]
fn abs_preserves_element_type_and_ieee_semantics() {
    let out = run_with_stdlib(
        r#"
        #include <math.hc>
        U0 Main() {
          "%d %d\n", Abs(-7), Abs(7);              // integers stay I64
          "%.2f %.2f\n", Abs(-3.5), Abs(3.5);       // floats are F64, not truncated
          "%.1f\n", Abs(-0.0);                       // IEEE: +0.0, not -0.0
          "%d\n", IsNaN(Abs(NaN()));                 // NaN stays NaN
        }
        Main;
    "#,
    );
    assert_eq!(out, "7 7\n3.50 3.50\n0.0\n1\n");
}

#[test]
fn strtoi64base_handles_bases_and_endptr() {
    let out = run_with_stdlib(
        r#"
        #include <stdlib.hc>
        U0 Main() {
          U8 *e;
          "%d %d %d %d %d\n",
            StrToI64Base("0xFF", 16, NULL),   // hex, explicit base
            StrToI64Base("0xff", 0, NULL),    // hex, auto-detected
            StrToI64Base("0755", 0, NULL),    // octal, auto-detected
            StrToI64Base("777", 8, NULL),     // octal, explicit
            StrToI64Base("-101", 2, NULL);    // binary, signed
          StrToI64Base("  42rest", 10, &e);   // endptr left just past the digits
          "endptr=[%s]\n", e;
          U8 *s = "zzz";                       // no digits: 0, endptr == start
          I64 v = StrToI64Base(s, 10, &e);
          "fail v=%d ateq=%d\n", v, e == s;
          "edge %d [%s]\n", StrToI64Base("0xZ", 0, &e), e; // "0x" w/o hex digit -> just "0"
          "compat %d %d %d\n", StrToI64("123"), StrToI64("  7x"), StrToI64("abc");
        }
        Main;
    "#,
    );
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
    let out = run_with_stdlib(
        r#"
        #include <stdlib.hc>
        U0 Main() {
          U8 *e;
          // strtoul: unsigned result, leading '-' wraps, full 64-bit range
          "%u %u\n", StrToU64Base("-1", 10, NULL),
                     StrToU64Base("0xFFFFFFFFFFFFFFFF", 16, NULL);
          StrToU64Base("  255zzz", 0, &e);
          "uend=[%s]\n", e;
          // strtod: value + endptr
          "%.3f\n", StrToF64End("3.14159xyz", &e);
          "fend=[%s]\n", e;
          "%.1f\n", StrToF64End("  -2.5e2 ", &e);
          U8 *s = "nope";                         // no digits: 0.0, endptr == start
          F64 v = StrToF64End(s, &e);
          "fail %.1f ateq=%d\n", v, e == s;
        }
        Main;
    "#,
    );
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
    let out = run_with_stdlib(
        r#"
        #include <limits.hc>
        #include <float.hc>
        #include <math.hc>
        U0 Main() {
          "%d %d %u | %d %d %u\n", I8_MIN, I8_MAX, U8_MAX, I16_MIN, I16_MAX, U16_MAX;
          "%d %d %u\n", I32_MIN, I32_MAX, U32_MAX;
          "%d %d %u\n", I64_MIN, I64_MAX, U64_MAX;
          // float characteristics must hit the canonical IEEE-754 bit patterns
          "%x %x %x %x\n", Float64bits(F64_MAX), Float64bits(F64_MIN),
                           Float64bits(F64_EPSILON), Float64bits(F64_TRUE_MIN);
          "%x %x\n", Float64bits(DBL_MAX), Float64bits(DBL_EPSILON); // C aliases
        }
        Main;
    "#,
    );
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
    let out = run_with_stdlib(
        r#"
        #include <stdio.hc>
        #include <string.hc>
        U0 Main() {
          U8 s[32]; StrCpy(s, "a,,b,");      // empty fields kept (unlike StrTok)
          U8 *p = s, *t;
          while ((t = StrSep(&p, ","))) "[%s]", t;
          "\n";
          U8 s2[32]; StrCpy(s2, ",x"); p = s2; // leading delimiter -> empty first field
          while ((t = StrSep(&p, ","))) "[%s]", t;
          "\n";
          U8 s3[32]; StrCpy(s3, "name=value"); p = s3;
          U8 *k = StrSep(&p, "="), *v = StrSep(&p, "=");
          "%s=%s null=%d\n", k, v, p == NULL;
        }
        Main;
    "#,
    );
    assert_eq!(out, "[a][][b][]\n[][x]\nname=value null=1\n");
}

#[test]
fn string_h_workhorses() {
    let out = run_with_stdlib(
        r#"
        #include <stdio.hc>
        #include <string.hc>
        U0 Main() {
          U8 buf[32]; StrCpy(buf, "ab"); StrNCat(buf, "cdef", 2);   // strncat
          "%s\n", buf;
          "%s\n", StrPBrk("hello, world", " ,");                     // strpbrk
          "%d %d %d\n", StrCaseCmp("Hello", "hello"), StrCaseCmp("abc", "abd"),
                        StrNCaseCmp("ABCxx", "abcyy", 3);            // strcasecmp/strncasecmp
          U8 *d = StrDup("dup"); "%s\n", d; Free(d);                 // strdup
          U8 *nd = StrNDup("truncated", 5); "%s\n", nd; Free(nd);    // strndup
          U8 s1[32]; StrCpy(s1, "a,bb,,ccc");                        // strtok (empty fields skipped)
          U8 *t = StrTok(s1, ",");
          while (t) { "%s.", t; t = StrTok(NULL, ","); }
          "\n";
          U8 s2[32]; StrCpy(s2, "one  two");                         // strtok_r
          U8 *sv; U8 *r = StrTokR(s2, " ", &sv);
          while (r) { "%s.", r; r = StrTokR(NULL, " ", &sv); }
          "\n";
          U8 mb[16]; U8 *q = MemCCpy(mb, "key=v", '=', 16); *q = 0;  // memccpy
          "%s\n", mb;
        }
        Main;
    "#,
    );
    assert_eq!(
        out,
        "abcd\n, world\n0 -1 0\ndup\ntrunc\na.bb.ccc.\none.two.\nkey=\n"
    );
}

#[test]
fn math_classification_and_integer_rounds() {
    let out = run_with_stdlib(
        r#"
        #include <stdio.hc>
        #include <math.hc>
        #include <float.hc>
        U0 Main() {
          // lround: ties away from zero
          "%d %d %d %d %d\n", LRound(2.5), LRound(-2.5), LRound(2.4), LRound(2.6), LRound(-0.5);
          // lrint: ties to even
          "%d %d %d %d\n", LRint(2.5), LRint(3.5), LRint(-2.5), LRint(2.6);
          F64 nan = NaN(), inf = Inf(1), sub = F64_TRUE_MIN;
          "%d %d %d %d\n", IsFinite(1.0), IsFinite(nan), IsFinite(inf), IsFinite(sub);
          "%d %d %d %d %d\n", IsNormal(1.0), IsNormal(0.0), IsNormal(sub), IsNormal(inf), IsNormal(nan);
          // FpClassify -> FP_NAN/INFINITE/ZERO/SUBNORMAL/NORMAL = 0..4
          "%d %d %d %d %d\n", FpClassify(nan), FpClassify(inf), FpClassify(0.0),
                              FpClassify(sub), FpClassify(1.5);
        }
        Main;
    "#,
    );
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
    let out = run_with_stdlib(
        r#"
        #include <stdio.hc>
        #include <time.hc>
        U0 Main() {
          "%.1f\n", Difftime(1000, 250);             // pure: 750.0 seconds
          DateTime l = Localtime(1700000000, -8 * 3600); // UTC 22:13:20 -> PST 14:13:20
          U8 b[64]; FmtISO(b, l); "%s\n", b;
          // CpuNS/Clock are impure -> property only: non-negative and non-decreasing
          I64 a = CpuNS(), s = 0, i;
          for (i = 0; i < 1000000; i++) s += i;
          I64 c = CpuNS();
          "%d %d %d\n", a >= 0, c >= a, Clock() >= 0;
          "%d\n", CLOCKS_PER_SEC;
        }
        Main;
    "#,
    );
    assert_eq!(out, "750.0\n2023-11-14 14:13:20\n1 1 1\n1000000\n");
}

#[test]
fn strftime_formats_dates() {
    let out = run_with_stdlib(
        r#"
        #include <stdio.hc>
        #include <time.hc>
        U0 Main() {
          DateTime dt = FromUnix(1700000000); // Tue 2023-11-14 22:13:20 UTC
          U8 b[128];
          Strftime(b, 128, "%Y-%m-%d %H:%M:%S", dt); "%s\n", b;
          Strftime(b, 128, "%a %A %b %B", dt); "%s\n", b;
          Strftime(b, 128, "%I:%M %p j=%j w=%w u=%u", dt); "%s\n", b;
          Strftime(b, 128, "%F %T %R %D %y %%", dt); "%s\n", b;
          Strftime(b, 128, "%c", dt); "%s\n", b;
          // truncation returns 0; a fitting one returns the length
          "trunc=%d ok=%d\n", Strftime(b, 5, "%Y-%m-%d", dt), Strftime(b, 8, "%H:%M", dt);
          DateTime e = FromUnix(0); Strftime(b, 128, "%a %F", e); "%s\n", b; // Thu epoch
        }
        Main;
    "#,
    );
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
    let out = run_with_stdlib(
        r#"
        #include <stdio.hc>
        #include <stdlib.hc>
        #include <math.hc>
        #include <string.hc>
        #include <errno.hc>
        U0 Main() {
          "%.1f %.1f %.1f\n", Fmax(2.5, 1.5), Fmin(2.5, 1.5), Fmax(5.0, NaN()); // fmin/fmax
          q, r := Div(-7, 2);                                   // div/ldiv, truncates to (-3,-1)
          "%d %d\n", q, r;
          "%d %d\n", StrNLen("hello", 3), StrNLen("hi", 9);     // strnlen
          PutChar('H'); PutChar('i'); PutChar('\n');             // putchar
          Puts("line");                                          // puts (+newline)
          "%s|%s\n", StrError(ECONNABORTED), StrError(ECANCELED); // new errno codes
        }
        Main;
    "#,
    );
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
          "%d %d %d %d\n", Gcd(48, 36), Factorial(6), Min(3, 9), Max(3, 9);
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
    // extra elementary functions. Values match a reference `strtod`/libm. The native
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
    // The error-function and gamma family. Values match libm/scipy to ~10 decimals.
    // The implementations are Taylor + continued-fraction erf, Winitzki+Newton
    // inverses, and a Lanczos g=7 gamma. The native backends are held to this exact
    // output by the conformance suites.
    let out = run_with_stdlib(
        r#"
        #include <math.hc>
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
    // J0/J1/Jn and Y0/Y1/Yn, spanning the small-x series and the large-x asymptotic.
    // The x=20 column crosses the threshold between them. Values match standard tables
    // to 10 decimals. The Wronskian J1·Y0 − J0·Y1 = 2/(πx) holds to ~1e-12 across the
    // range.
    let out = run_with_stdlib(
        r#"
        #include <math.hc>
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
    // `RandU64` (in `<stdlib.hc>`): the generator is deterministic. `SeedRand` makes the stream
    // reproducible — the same seed gives the same value — and seed-dependent: a
    // different seed gives a different value.
    let out = run_with_stdlib(
        r#"
        #include <stdlib.hc>
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
    // The pure-HolyC `atof`: a correctly-rounded bignum parser. Covers the Clinger
    // fast path, the exact bignum slow path (long significands, large/small exponents,
    // the smallest normal double), the `%g`-printed round-trip, atof-style prefix
    // stopping, and over/underflow. Each value is the IEEE double nearest the decimal,
    // bit-identical to a reference `strtod`. That equivalence was verified separately
    // against Python over a 438-input random battery.
    let out = run_with_stdlib(
        r#"
        #include <stdlib.hc>
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
        String::from("#include <stdio.hc>\n#include <math.hc>\nU0 Main(){\nU8 b[2048];\n");
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
        String::from("#include <stdio.hc>\n#include <math.hc>\nU0 Main(){\nU8 b[2048];\n");
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
        String::from("#include <stdio.hc>\n#include <math.hc>\nU0 Main(){\nU8 b[2048];\n");
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
fn realloc_preserves_contents() {
    // ReAlloc keeps the first min(old, new) bytes across a grow and a shrink, and a
    // NULL argument behaves like MAlloc. Pointer identity is implementation-defined:
    // in-place on a bump allocator, moved on libc/interp. So only the bytes are
    // asserted.
    let out = run_with_stdlib(
        r#"
        #include <string.hc>
        #include <stdlib.hc>
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
        #include <string.hc>
        #include <stdlib.hc>
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
    // and a deep clone (independent buffer). Monomorphized over every kind of element:
    // I64, F64, a pointer, and a class value. The class case round-trips a whole value
    // through the byte heap buffer, both on store (`VecPush`) and load (`VecAt`).
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
    // FromUnix/FmtISO/ToUnix over fixed epochs. These are pure, so the output is
    // reproducible. Covers the epoch, a leap year, and a pre-1970 negative timestamp.
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

#[test]
fn generic_value_param_array_runs() {
    // `int N` value parameter as an array dimension: `T data[N]` becomes `I64 data[4]`.
    let out = run_with_stdlib(
        r#"
        class Buf<type T, int N> { T data[N]; }
        U0 Main() {
            Buf<I64, 4> b;
            I64 i;
            for (i = 0; i < 4; i++) b.data[i] = i * 10;
            I64 s = 0;
            for (i = 0; i < 4; i++) s += b.data[i];
            "sum=%d size=%d\n", s, sizeof(Buf<I64, 4>);
        }
        Main;
    "#,
    );
    assert_eq!(out, "sum=60 size=32\n"); // 4*8 = 32 bytes; 0+10+20+30 = 60
}

#[test]
fn generic_type_switch_selects_and_discards() {
    // Each instantiation keeps only its arm. The `Pt` arm uses `v.x` (valid only when
    // T = Pt); for the I64/F64/U8* instantiations it is discarded before sema, so it
    // never errors. `Show("hi")` matches no arm and falls to `default`.
    let out = run_with_stdlib(
        r#"
        class Pt { I64 x; }
        U0 Show<type T>(T v) {
            switch type (T) {
                case I64: "int %d\n", v;
                case F64: "flt %.1f\n", v;
                case Pt:  "pt %d\n", v.x;
                default:  "other\n";
            }
        }
        U0 Main() {
            Show(7);
            Show(2.5);
            Pt p; p.x = 9; Show(p);
            Show("hi");
        }
        Main;
    "#,
    );
    assert_eq!(out, "int 7\nflt 2.5\npt 9\nother\n");
}
