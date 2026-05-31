//! Tests for the tree-walking interpreter backend.

use solomon::backend::Backend;
use solomon::backend::interp::{Interpreter, run_to_string};
use solomon::parser::parse;
use solomon::sema::check_program;

/// Parse, semantically check, then interpret `src`, returning captured output.
fn run(src: &str) -> String {
    let program = parse(src).unwrap_or_else(|e| panic!("parse failed: {e}"));
    let errs = check_program(&program);
    assert!(errs.is_empty(), "semantic errors: {errs:?}");
    run_to_string(&program).unwrap_or_else(|e| panic!("runtime error: {e}"))
}

#[test]
fn implicit_string_print() {
    assert_eq!(run(r#""Hello, World!\n";"#), "Hello, World!\n");
}

#[test]
fn formatted_print() {
    let src = r#"I64 x = 42; "x=%d\n", x;"#;
    assert_eq!(run(src), "x=42\n");
}

#[test]
fn format_specifiers() {
    let src = r#""%d %x %c %s %f\n", 255, 255, 65, "hi", 1.5;"#;
    assert_eq!(run(src), "255 ff A hi 1.500000\n");
}

#[test]
fn arithmetic_and_precedence() {
    assert_eq!(run(r#""%d\n", 1 + 2 * 3 - 4;"#), "3\n");
}

#[test]
fn float_arithmetic() {
    assert_eq!(run(r#""%f\n", 3.0 / 2.0;"#), "1.500000\n");
}

#[test]
fn integer_division_and_modulo() {
    assert_eq!(run(r#""%d %d\n", 17 / 5, 17 % 5;"#), "3 2\n");
}

#[test]
fn local_variables_and_assignment() {
    let src = r#"
        U0 Main() {
            I64 a = 10;
            a += 5;
            a *= 2;
            "%d\n", a;
        }
        Main;
    "#;
    assert_eq!(run(src), "30\n");
}

#[test]
fn recursion_factorial() {
    let src = r#"
        I64 Fact(I64 n) {
            if (n <= 1) return 1;
            return n * Fact(n - 1);
        }
        "%d\n", Fact(5);
    "#;
    assert_eq!(run(src), "120\n");
}

#[test]
fn fibonacci_loop() {
    let src = r#"
        I64 Fib(I64 n) {
            if (n < 2) return n;
            return Fib(n - 1) + Fib(n - 2);
        }
        U0 Main() {
            I64 i;
            for (i = 0; i < 8; i++)
                "%d ", Fib(i);
        }
        Main;
    "#;
    assert_eq!(run(src), "0 1 1 2 3 5 8 13 ");
}

#[test]
fn while_loop_accumulates() {
    let src = r#"
        I64 sum = 0, k = 1;
        while (k <= 5) {
            sum += k * k;
            k++;
        }
        "%d\n", sum;
    "#;
    assert_eq!(run(src), "55\n"); // 1+4+9+16+25
}

#[test]
fn do_while_runs_at_least_once() {
    let src = r#"
        I64 i = 10;
        do {
            "%d ", i;
            i++;
        } while (i < 10);
    "#;
    assert_eq!(run(src), "10 ");
}

#[test]
fn ternary_and_logical_short_circuit() {
    assert_eq!(run(r#""%d\n", (3 > 2 && 1) ? 7 : 9;"#), "7\n");
}

#[test]
fn bitwise_operations() {
    assert_eq!(
        run(r#""%d %d %d\n", 12 & 10, 12 | 3, 1 << 4;"#),
        "8 15 16\n"
    );
}

#[test]
fn right_shift_is_signedness_directed() {
    // `>>` is arithmetic for a signed left operand (default I64) and logical for
    // an unsigned one — the same rule the native backend uses (tests/arm64.rs).
    let src = r#"
        U0 Main() {
            I64 a = -8;
            U64 u = 0x8000000000000000;
            I64 c = -64;
            c >>= 2;
            "%d %d %x %d\n", a >> 1, (-32) >> 2, u >> 4, c;
        }
        Main;
    "#;
    assert_eq!(run(src), "-4 -8 800000000000000 -16\n");
}

#[test]
fn division_is_signedness_directed() {
    // `/` and `%` are signed for a signed left operand, unsigned otherwise — the
    // same rule the native backend uses (tests/arm64.rs).
    let src = r#"
        U0 Main() {
            U64 u = 0x8000000000000000;
            I64 s = -17;
            U64 c = 100;
            c /= 8;
            c %= 9;
            "%x %d %d %d %d\n", u / 2, u % 7, s / 5, s % 5, c;
        }
        Main;
    "#;
    assert_eq!(run(src), "4000000000000000 1 -3 -2 3\n");
}

#[test]
fn comparison_is_signedness_directed_and_exact() {
    // Relational compares are unsigned when either operand is unsigned, and both
    // `<`-family and `==` compare integers at full 64-bit width (no f64 rounding
    // past 2^53) — matching the native backend (tests/arm64.rs).
    let src = r#"
        U0 Main() {
            U64 u = 0x8000000000000000;
            I64 s = -1;
            I64 a = 9007199254740993;
            I64 b = 9007199254740992;
            "%d %d %d %d %d %d\n",
                u > 1, u < 1, s < u, a > b, a == b, -5 < 3;
        }
        Main;
    "#;
    assert_eq!(run(src), "1 0 0 1 0 1\n");
}

#[test]
fn narrow_integer_types_wrap_on_store_arg_and_return() {
    // Narrow types promote to I64 for arithmetic (no mid-expression wrap), then
    // truncate to width on store, on argument passing, and on return — matching
    // the native backend (tests/arm64.rs).
    let src = r#"
        U8 AddU8(U8 a, U8 b) { return a + b; }
        I8 AddI8(I8 a, I8 b) { return a + b; }
        U0 Main() {
            U8 x = 200; x = x + 100;          // 300 -> 44 on store
            I8 s = 100; s = s + 100;          // 200 -> -56
            U8 a = 200; I64 wide = a + 100;   // 300, no wrap (I64 lvalue)
            U8 c = 250; c += 10;              // 260 -> 4
            "%d %d %d %d %d %d\n",
                x, s, wide, AddU8(300, 0), AddU8(200, 100), AddI8(100, 100);
        }
        Main;
    "#;
    // x=44, s=-56, wide=300, AddU8(300,0): arg 300->44 ->44, AddU8(200,100): 300->44,
    // AddI8(100,100): 200->-56.
    assert_eq!(run(src), "44 -56 300 44 44 -56\n");
}

#[test]
fn strprint_formats_into_a_buffer() {
    // `StrPrint(dst, fmt, ...)` formats into dst (the full printf grammar) and
    // returns dst — matching the native `sprintf` lowering (tests/arm64.rs).
    let src = r#"
        U0 Main() {
            U8 *buf = MAlloc(128);
            U8 *r = StrPrint(buf, "x=%d hex=%05x pi=%.2f e=%.3e s=%s", 42, 255, 3.14159, 1.5, "hi");
            "%s ret_eq=%d\n", buf, r == buf;
            StrPrint(buf, "%g/%g", 0.0001, 1000000.0);
            "%s\n", buf;
            Free(buf);
        }
        Main;
    "#;
    assert_eq!(
        run(src),
        "x=42 hex=000ff pi=3.14 e=1.500e+00 s=hi ret_eq=1\n0.0001/1e+06\n"
    );
}

#[test]
fn memsearch_and_number_to_str() {
    // MemSearch (memmem) locates a byte sequence; I64ToStr / F64ToStr format a
    // number into a buffer. All match the native backend (tests/arm64.rs).
    let src = r#"
        U0 Main() {
            U8 *hay = MAlloc(32); StrCpy(hay, "the quick brown fox");
            "%d %d\n", MemSearch(hay, 19, "quick", 5) - hay, MemSearch(hay, 19, "zzz", 3) == NULL;
            U8 *buf = MAlloc(32);
            "%s\n", I64ToStr(-12345, buf);
            "%s\n", F64ToStr(3.14, buf);
            "%s\n", F64ToStr(1000000.0, buf);
            Free(hay); Free(buf);
        }
        Main;
    "#;
    assert_eq!(run(src), "4 1\n-12345\n3.14\n1e+06\n");
}

#[test]
fn strspn_and_strcspn() {
    // StrSpn/StrCSpn measure the leading run of chars in / not in a set. Matches
    // the native backend (tests/arm64.rs).
    let src = r#"
        U0 Main() {
            U8 *a = MAlloc(32); StrCpy(a, "123abc456");
            U8 *b = MAlloc(8); StrCpy(b, "xyz");
            "%d %d %d %d\n",
                StrSpn(a, "0123456789"), StrCSpn(a, "abc"),
                StrSpn(b, "0"), StrCSpn(b, "0");
            Free(a); Free(b);
        }
        Main;
    "#;
    assert_eq!(run(src), "3 3 0 3\n");
}

#[test]
fn strchr_and_strlastchr() {
    // StrChr/StrLastChr find the first/last char (the NUL counts, so c==0 finds
    // the terminator), else NULL. Matches the native backend (tests/arm64.rs).
    let src = r#"
        U0 Main() {
            U8 *p = MAlloc(32); StrCpy(p, "/usr/local/bin");
            "%d %d %s\n", StrChr(p, '/') - p, StrLastChr(p, '/') - p, StrLastChr(p, '/') + 1;
            "%d %d\n", StrChr(p, 'Z') == NULL, StrChr(p, 0) - p;
            Free(p);
        }
        Main;
    "#;
    assert_eq!(run(src), "0 10 bin\n1 14\n");
}

#[test]
fn strrev_and_memfind() {
    // StrRev reverses in place (incl. empty/single-char); MemFind locates a byte
    // or returns NULL. Both match the native backend (tests/arm64.rs).
    let src = r#"
        U0 Main() {
            U8 *s = MAlloc(16); StrCpy(s, "Hello");
            "%s\n", StrRev(s);
            U8 *one = MAlloc(4); StrCpy(one, "x");
            "%s\n", StrRev(one);
            U8 *buf = MAlloc(16); StrCpy(buf, "abcdef");
            "%d %d\n", MemFind(buf, 'c', 6) - buf, MemFind(buf, 'z', 6) == NULL;
            Free(s); Free(one); Free(buf);
        }
        Main;
    "#;
    assert_eq!(run(src), "olleH\nx\n2 1\n");
}

#[test]
fn str_case_str2f64_and_memmove() {
    // StrToF64 (atof-style parse), in-place StrToUpper/StrToLower, and an
    // overlapping MemMove — all matching the native backend (tests/arm64.rs).
    let src = r#"
        U0 Main() {
            "%.3f %.3f %.3f\n", StrToF64("3.14"), StrToF64("-2.5e2"), StrToF64("  6.0x");
            U8 *s = MAlloc(32); StrCpy(s, "Hello, World 42!");
            "%s\n", StrToUpper(s);
            "%s\n", StrToLower(s);
            U8 *a = MAlloc(16); StrCpy(a, "abcdef");
            MemMove(a + 2, a, 4);
            "%s ret=%d\n", a, StrToUpper(s) == s;
            Free(s); Free(a);
        }
        Main;
    "#;
    assert_eq!(
        run(src),
        "3.140 -250.000 6.000\nHELLO, WORLD 42!\nhello, world 42!\nababcd ret=1\n"
    );
}

#[test]
fn mstrprint_and_str2i64() {
    // MStrPrint formats into a fresh right-sized buffer; StrToI64 parses base-10
    // like atoll (whitespace/sign, stops at non-digit). Both match the native
    // backend (tests/arm64.rs).
    let src = r#"
        U0 Main() {
            U8 *s = MStrPrint("n=%d hex=%x pi=%.2f", 42, 255, 3.14159);
            "%s len=%d\n", s, StrLen(s);
            Free(s);
            "%d %d %d %d\n", StrToI64("123"), StrToI64("-45"), StrToI64("  7x"), StrToI64("abc");
        }
        Main;
    "#;
    assert_eq!(run(src), "n=42 hex=ff pi=3.14 len=19\n123 -45 7 0\n");
}

#[test]
fn catprint_appends_to_a_buffer() {
    // `CatPrint` appends (formats at dst + StrLen(dst)); matches the native
    // `sprintf(dst + strlen(dst), ...)` lowering (tests/arm64.rs).
    let src = r#"
        U0 Main() {
            U8 *buf = MAlloc(256);
            StrPrint(buf, "Items:");
            I64 i;
            for (i = 1; i <= 3; i++)
                CatPrint(buf, " #%d=%d", i, i * i);
            U8 *r = CatPrint(buf, " | total=%.1f", 14.0);
            "%s\nret_eq=%d len=%d\n", buf, r == buf, StrLen(buf);
            Free(buf);
        }
        Main;
    "#;
    assert_eq!(
        run(src),
        "Items: #1=1 #2=4 #3=9 | total=14.0\nret_eq=1 len=34\n"
    );
}

#[test]
fn printf_scientific_and_general_floats() {
    // `%e`/`%E` (scientific) and `%g`/`%G` (general) match libc / the native
    // backend (tests/arm64.rs), including rounding carry, style choice, the `#`
    // flag, and width/justify/sign.
    let src = r#"
        U0 Main() {
            "[%e][%E][%.2e]\n", 1.5, 1234.5, 9.9999996;
            "[%g][%g][%g][%.3g][%#g]\n", 1.5, 1000000.0, 0.0001, 1234567.0, 1.5;
            "[%12.3e][%-12.3e][%+g]\n", 1.5, 1.5, 2.5;
        }
        Main;
    "#;
    assert_eq!(
        run(src),
        "[1.500000e+00][1.234500E+03][1.00e+01]\n\
         [1.5][1e+06][0.0001][1.23e+06][1.50000]\n\
         [   1.500e+00][1.500e+00   ][+2.5]\n"
    );
}

#[test]
fn float_to_unsigned_uses_unsigned_conversion() {
    // A float past I64::MAX converts to U64 via the unsigned path (fcvtzu) — not
    // signed saturation — across init/cast/assign/return, while a signed target
    // still saturates and a negative float clamps to 0. Matches tests/arm64.rs.
    let src = r#"
        U64 Ret(F64 f) { return f; }
        U0 Main() {
            U64 big = 1.0e19;
            U64 cast = (U64)1.0e19;
            I64 sbig = 1.0e19;
            U64 a; a = 1.5e19;
            F64 neg = -1.0; U64 n = neg;
            "%u %u %d %u %u %u\n", big, cast, sbig, a, n, Ret(1.2e19);
        }
        Main;
    "#;
    assert_eq!(
        run(src),
        "10000000000000000000 10000000000000000000 9223372036854775807 \
         15000000000000000000 0 12000000000000000000\n"
    );
}

#[test]
fn printf_flags_width_precision_octal() {
    // The formatter honors flags/width/precision/octal/length and treats values
    // as 64-bit, matching libc (and the native backend in tests/arm64.rs).
    let src = r#"
        U0 Main() {
            U8 *s = "hello";
            "[%5d][%-5d][%05x][%+d][%#x][%.3d][%o]\n", 42, 42, 255, 7, 255, 5, 64;
            "[%10s][%-10.2s][%c]\n", s, s, 65;
            "[%x][%u]\n", -1, -1;
        }
        Main;
    "#;
    assert_eq!(
        run(src),
        "[   42][42   ][000ff][+7][0xff][005][100]\n\
         [     hello][he        ][A]\n\
         [ffffffffffffffff][18446744073709551615]\n"
    );
}

#[test]
fn switch_with_fallthrough_and_range() {
    let src = r#"
        U0 Classify(I64 v) {
            switch (v) {
                case 0:
                    "zero ";
                case 1 ... 3:
                    "small\n";
                    break;
                default:
                    "other\n";
            }
        }
        Classify(0);
        Classify(2);
        Classify(9);
    "#;
    // v=0 falls through into the 1..3 case; v=2 hits the range; v=9 default.
    assert_eq!(run(src), "zero small\nsmall\nother\n");
}

#[test]
fn switch_start_end_prologue_and_epilogue() {
    // HolyC `start:` runs on entry (before dispatch); `end:` is the fall-through
    // epilogue that a `break` skips.
    let src = r#"
        I64 Classify(I64 n) {
            I64 r = 0;
            switch [n] {
                start:
                    r = r + 100;
                case 1:
                    r = r + 1;
                    break;
                case 2:
                    r = r + 2;
                end:
                    r = r + 1000;
            }
            return r;
        }
        U0 Main() {
            "%d %d %d\n", Classify(1), Classify(2), Classify(9);
        }
        Main;
    "#;
    // 1: prologue+case1, break -> 101. 2: prologue+case2, fall into epilogue ->
    // 1102. 9: prologue, no match, jump to epilogue -> 1100.
    assert_eq!(run(src), "101 1102 1100\n");
}

#[test]
fn switch_dense_cases_with_gaps_and_range() {
    // A dense switch the native backend lowers to a branch table — exercise a
    // gap (3), a range (5..7), and out-of-range values (-1, 8, 9) -> default.
    // The interpreter is the oracle for the native twin in tests/arm64.rs.
    let src = r#"
        I64 Name(I64 d) {
            I64 r;
            switch (d) {
                case 0: r = 10; break;
                case 1: r = 11; break;
                case 2: r = 12; break;
                case 4: r = 14; break;
                case 5 ... 7: r = 57; break;
                default: r = 99;
            }
            return r;
        }
        U0 Main() {
            I64 i;
            for (i = -1; i <= 9; i++)
                "%d ", Name(i);
            "\n";
        }
        Main;
    "#;
    assert_eq!(run(src), "99 10 11 12 99 14 57 57 57 99 99 \n");
}

#[test]
fn switch_with_constant_folded_case_values() {
    // Case labels built from constant arithmetic must dispatch by their folded
    // value (the native backend folds these for its jump table; this pins the
    // expected values the table is checked against in tests/arm64.rs).
    let src = r#"
        I64 Pick(I64 d) {
            I64 r;
            switch (d) {
                case 3 - 3: r = 10; break;
                case 4 / 4: r = 11; break;
                case 1 + 1: r = 12; break;
                case 9 / 3: r = 13; break;
                case 2 * 2: r = 14; break;
                case 1 << 2 | 1: r = 15; break;
                default: r = -1;
            }
            return r;
        }
        U0 Main() {
            I64 i;
            for (i = -1; i <= 6; i++) "%d ", Pick(i);
            "\n";
        }
        Main;
    "#;
    assert_eq!(run(src), "-1 10 11 12 13 14 15 -1 \n");
}

#[test]
fn structs_and_pointers() {
    let src = r#"
        class Point { I64 x; I64 y; }
        U0 Main() {
            Point p;
            p.x = 3;
            p.y = 4;
            Point *pp = &p;
            pp->x = pp->x + 10;
            "%d %d\n", p.x, p.y;
        }
        Main;
    "#;
    // The pointer write is visible through `p`.
    assert_eq!(run(src), "13 4\n");
}

#[test]
fn arrays_index_and_mutate() {
    let src = r#"
        U0 Main() {
            I64 a[3];
            a[0] = 5;
            a[1] = a[0] * 2;
            a[2] = a[0] + a[1];
            "%d %d %d\n", a[0], a[1], a[2];
        }
        Main;
    "#;
    assert_eq!(run(src), "5 10 15\n");
}

#[test]
fn default_arguments() {
    let src = r#"
        I64 AddN(I64 a, I64 b = 100) { return a + b; }
        "%d %d\n", AddN(1), AddN(1, 2);
    "#;
    assert_eq!(run(src), "101 3\n");
}

#[test]
fn goto_and_labels() {
    let src = r#"
        U0 Main() {
            I64 i = 0;
        top:
            "%d ", i;
            i++;
            if (i < 3) goto top;
        }
        Main;
    "#;
    assert_eq!(run(src), "0 1 2 ");
}

#[test]
fn cast_truncates_float() {
    assert_eq!(run(r#""%d\n", (I64)3.9;"#), "3\n");
}

#[test]
fn sizeof_type() {
    assert_eq!(
        run(r#""%d %d %d\n", sizeof(I64), sizeof(U8), sizeof(F64);"#),
        "8 1 8\n"
    );
}

#[test]
fn sizeof_class_uses_layout() {
    // Point is two I64s => 16 bytes; the layout pass feeds sizeof.
    let src = r#"
        class Point { I64 x; I64 y; }
        "%d\n", sizeof(Point);
    "#;
    assert_eq!(run(src), "16\n");
}

#[test]
fn sizeof_expression_uses_inferred_type() {
    // sizeof of an expression resolves via its statically inferred type:
    //   x       => I64   (8)
    //   1.5     => F64   (8)
    //   x > 0   => I64   (8, a comparison)
    //   p.y     => I32   (4, a struct field)
    let src = r#"
        class Rec { U8 tag; I32 y; }
        U0 Main() {
            I64 x = 0;
            Rec p;
            "%d %d %d %d\n", sizeof(x), sizeof(1.5), sizeof(x > 0), sizeof(p.y);
        }
        Main;
    "#;
    assert_eq!(run(src), "8 8 8 4\n");
}

#[test]
fn sizeof_array_variable_does_not_decay() {
    // `sizeof` of an array variable is the whole array (no pointer decay),
    // while an element is the element size.
    let src = r#"
        U0 Main() {
            I64 a[4];
            "%d %d\n", sizeof(a), sizeof(a[0]);
        }
        Main;
    "#;
    assert_eq!(run(src), "32 8\n");
}

#[test]
fn offset_of_class_members() {
    // `offset(Class.field)` is the byte offset from the layout pass, including
    // padding and nested member paths.
    let src = r#"
        class Pt { I64 x; I64 y; }
        class Box { Pt lo; Pt hi; }
        class Mixed { U8 a; I32 b; I64 c; }
        U0 Main() {
            "%d %d\n", offset(Pt.x), offset(Pt.y);
            "%d %d %d\n", offset(Box.lo), offset(Box.hi), offset(Box.hi.y);
            "%d %d\n", offset(Mixed.b), offset(Mixed.c);
        }
        Main;
    "#;
    assert_eq!(run(src), "0 8\n0 16 24\n4 8\n");
}

#[test]
fn aggregate_initializers() {
    let src = r#"
        class Pt { I64 x; I64 y; }
        U0 Main() {
            I64 a[] = {10, 20, 30};
            "%d %d %d %d\n", a[0], a[1], a[2], sizeof(a);
            I64 b[5] = {1, 2};
            "%d %d %d %d %d\n", b[0], b[1], b[2], b[3], b[4];
            Pt p = {7, 8};
            "%d %d\n", p.x, p.y;
            I64 m[2][3] = {{1, 2, 3}, {4, 5, 6}};
            "%d %d %d\n", m[0][0], m[1][1], m[1][2];
            Pt ps[2] = {{1, 2}, {3, 4}};
            "%d %d %d %d\n", ps[0].x, ps[0].y, ps[1].x, ps[1].y;
        }
        Main;
    "#;
    assert_eq!(run(src), "10 20 30 24\n1 2 0 0 0\n7 8\n1 5 6\n1 2 3 4\n");
}

#[test]
fn global_aggregate_initializers() {
    let src = r#"
        class Pt { I64 x; I64 y; }
        I64 g[] = {5, 6, 7};
        Pt gp = {3, 4};
        U0 Main() {
            "%d %d %d %d %d\n", g[0], g[1], g[2], gp.x, gp.y;
        }
        Main;
    "#;
    assert_eq!(run(src), "5 6 7 3 4\n");
}

#[test]
fn designated_initializers() {
    let src = r#"
        class Pt { I64 x; I64 y; }
        class Line { Pt a; Pt b; I64 tag; }
        U0 Main() {
            Pt p = {.x = 1, .y = 2};
            "%d %d\n", p.x, p.y;
            Pt q = {.y = 7, .x = 3};       // out of order
            "%d %d\n", q.x, q.y;
            Pt r = {.y = 9};               // partial: x defaults to 0
            "%d %d\n", r.x, r.y;
            Line l = {.tag = 42, .b = {.x = 5, .y = 6}};  // nested, a left default
            "%d %d %d %d %d\n", l.a.x, l.a.y, l.b.x, l.b.y, l.tag;
        }
        Main;
    "#;
    assert_eq!(run(src), "1 2\n3 7\n0 9\n0 0 5 6 42\n");
}

#[test]
fn global_designated_initializers() {
    let src = r#"
        class Pt { I64 x; I64 y; }
        Pt gp = {.y = 8, .x = 4};
        U0 Main() {
            "%d %d\n", gp.x, gp.y;
        }
        Main;
    "#;
    assert_eq!(run(src), "4 8\n");
}

#[test]
fn initializer_element_types() {
    // Initializers cover floats, string/pointer fields, char literals, constant
    // expressions, and a trailing comma — positional and designated alike.
    let src = r#"
        class V { F64 x; F64 y; }
        class S { U8 *name; I64 n; }
        U0 Main() {
            F64 fs[3] = {1.5, 2.5};               // partial float array
            "%f %f %f\n", fs[0], fs[1], fs[2];
            V v = {.y = 4.5};                     // designated float field
            "%f %f\n", v.x, v.y;
            S s = {.name = "hi", .n = 3};         // string + int fields
            "%s %d\n", s.name, s.n;
            I64 e[2] = {1 + 2, 3 * 4,};           // expression values + trailing comma
            "%d %d\n", e[0], e[1];
            U8 cs[4] = {'a', 'b', 'c'};           // char literals into bytes
            "%d %d %d %d\n", cs[0], cs[1], cs[2], cs[3];
        }
        Main;
    "#;
    assert_eq!(
        run(src),
        "1.500000 2.500000 0.000000\n0.000000 4.500000\nhi 3\n3 12\n97 98 99 0\n"
    );
}

#[test]
fn array_of_structs_designated() {
    // A positional array whose elements are designated struct initializers.
    let src = r#"
        class Pt { I64 x; I64 y; }
        U0 Main() {
            Pt ps[2] = {{.x = 1, .y = 2}, {.y = 4}};
            "%d %d %d %d\n", ps[0].x, ps[0].y, ps[1].x, ps[1].y;
        }
        Main;
    "#;
    assert_eq!(run(src), "1 2 0 4\n");
}

#[test]
fn member_access_on_call_result() {
    let src = r#"
        class Pt { I64 x; I64 y; }
        class Box { Pt lo; Pt hi; }
        Pt Mk(I64 a, I64 b) { Pt p = {.x = a, .y = b}; return p; }
        Box MkBox() { Box r = {.lo = {.x = 1, .y = 2}, .hi = {.x = 3, .y = 4}}; return r; }
        U0 Main() {
            "%d %d\n", Mk(3, 4).x, Mk(3, 4).y;     // scalar fields off a call
            "%d %d\n", MkBox().lo.y, MkBox().hi.x; // nested member off a call
            Pt got = MkBox().hi;                   // member-on-call yielding a struct
            "%d %d\n", got.x, got.y;
            "%d\n", Mk(10, 20).x + Mk(10, 20).y;   // used inside an expression
        }
        Main;
    "#;
    assert_eq!(run(src), "3 4\n2 3\n3 4\n30\n");
}

#[test]
fn function_pointers() {
    let src = r#"
        I64 Add(I64 a, I64 b) { return a + b; }
        I64 Mul(I64 a, I64 b) { return a * b; }
        I64 Reduce(I64 (*op)(I64, I64), I64 a, I64 b, I64 c) {
            return op(op(a, b), c);
        }
        U0 Main() {
            I64 (*fp)(I64, I64) = &Add;
            "%d\n", fp(3, 4);
            fp = &Mul;                    // reassign
            "%d\n", fp(3, 4);
            "%d %d\n", Reduce(&Add, 1, 2, 3), Reduce(&Mul, 2, 3, 4);  // callbacks
        }
        Main;
    "#;
    assert_eq!(run(src), "7\n12\n6 24\n");
}

#[test]
fn function_pointer_dispatch_table_and_vtable() {
    let src = r#"
        I64 Add(I64 a, I64 b) { return a + b; }
        I64 Sub(I64 a, I64 b) { return a - b; }
        I64 Mul(I64 a, I64 b) { return a * b; }
        I64 SqArea(I64 s) { return s * s; }
        class Shape { I64 (*area)(I64); I64 size; }
        U0 Main() {
            I64 (*ops[])(I64, I64) = {&Add, &Sub, &Mul};   // dispatch table
            I64 i;
            for (i = 0; i < 3; i++) "%d ", ops[i](10, 3);
            "\n";
            Shape sq = {&SqArea, 5};                       // vtable-style struct
            "%d\n", sq.area(sq.size);
        }
        Main;
    "#;
    assert_eq!(run(src), "13 7 30 \n25\n");
}

#[test]
fn typedef_aliases() {
    // A function-pointer typedef (`BinOp`), including a function that returns one
    // — the readable form of the otherwise-arcane declarator.
    let src = r#"
        typedef I64 (*BinOp)(I64, I64);
        typedef U8 *String;
        I64 Add(I64 a, I64 b) { return a + b; }
        I64 Mul(I64 a, I64 b) { return a * b; }
        BinOp Pick(I64 which) { if (which) return &Mul; return &Add; }
        U0 Main() {
            BinOp op = &Add;
            "%d\n", op(20, 22);
            "%d\n", Pick(1)(6, 7);
            String s = "hi";
            "%s\n", s;
        }
        Main;
    "#;
    assert_eq!(run(src), "42\n42\nhi\n");
}

#[test]
fn unions_share_storage() {
    // A union's fields overlap a single byte buffer, so writing one field and
    // reading another sees the same bytes (type punning), matching the native
    // backend. Also covers value-semantic copy and brace/designated init.
    let src = r#"
        union U { I64 whole; U8 b[8]; F64 d; }
        U0 Main() {
            U u;
            u.b[0] = 0x44; u.b[1] = 0x43; u.b[2] = 0x42; u.b[3] = 0x41;
            u.b[4] = 0; u.b[5] = 0; u.b[6] = 0; u.b[7] = 0;
            "%d\n", u.whole;                  // 0x41424344, read via the scalar
            u.whole = 0x4142;
            "%d %d\n", u.b[0], u.b[1];         // 0x42 0x41, read via the bytes
            U v = u;                          // value-semantic copy
            v.whole = 0;
            "%d %d\n", u.whole, v.whole;       // independent storage
            U a = {0x4142};                   // brace init sets the first member
            "%d %d\n", a.b[0], a.b[1];
            U c = {.whole = 7};               // designated init
            "%d\n", c.whole;
        }
        Main;
    "#;
    assert_eq!(run(src), "1094861636\n66 65\n16706 0\n66 65\n7\n");
}

#[test]
fn anonymous_union_members_are_promoted() {
    // An anonymous `union {...}` in a class promotes its members: they are
    // accessed directly (`r.whole`) and overlap, matching the native layout.
    let src = r#"
        class Reg {
          I64 tag;
          union { I64 whole; U8 b[8]; F64 d; };
          union { I32 lo; I32 hi; };
        }
        U0 Main() {
          Reg r;
          r.tag = 9;
          r.whole = 0;
          r.b[0] = 0x44; r.b[1] = 0x43;      // write via a promoted array
          "%d %d\n", r.tag, r.whole;          // read via a promoted scalar
          r.d = 1.5;
          "%d\n", r.whole;                    // exact F64 bit pattern
          r.lo = 7;                           // a second, independent anon union
          "%d %d\n", r.lo, r.tag;
        }
        Main;
    "#;
    assert_eq!(run(src), "9 17220\n4609434218613702656\n7 9\n");
}

#[test]
fn named_union_member_embedded_in_class() {
    // Named embedded unions: an inline definition with a member, and a member of
    // a previously-defined union. Accessed as `box.b.field` (not promoted).
    let src = r#"
        union Val { I64 i; F64 f; }
        class Box {
          I64 tag;
          union Bits { I64 whole; U8 bytes[8]; } b;
          union Val v;
        }
        U0 Main() {
          Box box;
          box.tag = 1;
          box.b.whole = 0x41424344;
          box.v.f = 2.5;
          "%d %d %d %d\n", box.tag, box.b.bytes[0], box.b.bytes[3], box.v.i != 0;
        }
        Main;
    "#;
    assert_eq!(run(src), "1 68 65 1\n");
}

#[test]
fn string_memory_and_math_builtins() {
    let src = r#"
        U0 Main() {
            U8 *s = MAlloc(32);
            StrCpy(s, "Hello, ");
            StrCat(s, "World");
            "%s %d\n", s, StrLen(s);
            U8 *d = MAlloc(8);
            MemSet(d, '*', 3); d[3] = 0;
            "%s\n", d;
            MemCpy(d, "xy", 2); d[2] = 0;
            "%s\n", d;
            "%d %d %d\n", ToUpper('a'), ToLower('Z'), ToUpper('!');  // '!' unchanged
            "%d %d %d\n", Sin(0.0) == 0.0, Cos(0.0) == 1.0, Pow(2.0, 10.0) == 1024.0;
            Free(s); Free(d);
        }
        Main;
    "#;
    assert_eq!(run(src), "Hello, World 12\n***\nxy\n65 122 33\n1 1 1\n");
}

#[test]
fn memcmp_and_more_math_builtins() {
    let src = r#"
        U0 Main() {
            "%d %d %d\n", MemCmp("abc", "abc", 3), MemCmp("abc", "abd", 3), MemCmp("abd", "abc", 3);
            "%d %d %d\n", (I64)Floor(3.7), (I64)Ceil(3.2), (I64)Round(3.5);
            "%d %d\n", (I64)Floor(-3.2), (I64)Ceil(-3.7);
            "%d %d %d\n", Exp(0.0) == 1.0, Ln(1.0) == 0.0, Tan(0.0) == 0.0;
        }
        Main;
    "#;
    assert_eq!(run(src), "0 -1 1\n3 4 4\n-4 -3\n1 1 1\n");
}

#[test]
fn strfind_and_inverse_trig_builtins() {
    let src = r#"
        U0 Main() {
            U8 *s = MAlloc(16);
            StrCpy(s, "abcdef");
            U8 *p = StrFind(s, "cd");       // pointer into the buffer at index 2
            "%d %d %d\n", p != NULL, p - s, StrFind(s, "zz") == NULL;
            "%d %d %d\n", (I64)(ASin(1.0) * 100), (I64)(ATan2(1.0, 1.0) * 100), (I64)Log10(1000.0);
            Free(s);
        }
        Main;
    "#;
    assert_eq!(run(src), "1 2 1\n157 78 3\n");
}

#[test]
fn strn_sign_fabs_builtins() {
    let src = r#"
        U0 Main() {
            "%d %d %d\n", StrNCmp("abc", "abd", 2), StrNCmp("abc", "abd", 3), StrNCmp("abc", "abc", 3);
            U8 *d = MAlloc(8);
            StrNCpy(d, "hello", 3); d[3] = 0;
            "%s\n", d;
            "%d %d %d\n", Sign(-42), Sign(0), Sign(99);
            "%d %d\n", (I64)Fabs(-3.5), (I64)Fabs(3.5);
            Free(d);
        }
        Main;
    "#;
    assert_eq!(run(src), "0 -1 0\nhel\n-1 0 1\n3 3\n");
}

#[test]
fn string_literals_index_like_pointers() {
    // A bare string literal decays to a `U8*`, so it can be indexed/dereferenced.
    let src = r#"
        I64 SumChars(U8 *s) {
            I64 sum = 0;
            I64 i = 0;
            while (s[i] != 0) { sum += s[i]; i++; }
            return sum;
        }
        U0 Main() {
            "%d %d %d\n", "abc"[0], *"xyz", SumChars("AB");  // 97, 120, 65+66=131
        }
        Main;
    "#;
    assert_eq!(run(src), "97 120 131\n");
}

#[test]
fn randu64_is_deterministic() {
    // splitmix64 from a zero seed — the identical sequence the native backend
    // produces (it shares `builtins::splitmix64`).
    let src = r#"
        U0 Main() {
            I64 i;
            for (i = 0; i < 4; i++) "%d\n", RandU64() & 0xFFFF;
        }
        Main;
    "#;
    assert_eq!(run(src), "52655\n26100\n17743\n33260\n");
}

#[test]
fn print_builtin() {
    assert_eq!(run(r#"Print("n=%d\n", 7);"#), "n=7\n");
}

#[test]
fn stdlib_builtins() {
    let src = r#"
        U0 Main() {
            "%d %d\n", Abs(-7), Abs(5);                  // integer absolute value
            "%d %d\n", StrLen("hello"), StrLen("");      // string length
            "%d %d\n", Sqrt(16.0) == 4.0, Sqrt(9.0) == 3.0;  // square root (F64)
        }
        Main;
    "#;
    assert_eq!(run(src), "7 5\n5 0\n1 1\n");
}

#[test]
fn float_to_int_conversion_truncates() {
    // An F64 value assigned/returned into an integer truncates toward zero.
    let src = r#"
        I64 Trunc(F64 x) { return x; }
        U0 Main() {
            I64 a = 16.0;
            I64 b = -3.9;
            I64 c; c = 2.7;
            "%d %d %d %d %d\n", a, b, c, Trunc(7.9), Trunc(-7.9);
        }
        Main;
    "#;
    assert_eq!(run(src), "16 -3 2 7 -7\n");
}

#[test]
fn string_and_heap_builtins() {
    let src = r#"
        U0 Main() {
            "%d %d %d\n", StrCmp("abc", "abc"), StrCmp("abc", "abd"), StrCmp("abd", "abc");
            "%d %d\n", StrCmp("ab", "abc"), StrCmp("abc", "ab");  // prefix ordering
            U8 *buf = MAlloc(16);
            StrCpy(buf, "hello");
            "%s %d\n", buf, StrLen(buf);
            StrCpy(buf, "hi");                 // overwrite
            "%s %d\n", buf, StrCmp(buf, "hi");
            Free(buf);
        }
        Main;
    "#;
    assert_eq!(run(src), "0 -1 1\n-1 1\nhello 5\nhi 0\n");
}

#[test]
fn malloc_of_typed_structs() {
    // A heap-allocated array of structs: the buffer holds struct elements, so
    // `ps[i].field` works (matching the native byte-addressed heap).
    let src = r#"
        class Pt { I64 x; I64 y; }
        U0 Main() {
            Pt *ps = MAlloc(sizeof(Pt) * 3);
            ps[0].x = 1; ps[0].y = 2;
            ps[2].x = 5; ps[2].y = 6;
            "%d %d %d %d\n", ps[0].x, ps[0].y, ps[2].x, ps[2].y;
            Free(ps);
        }
        Main;
    "#;
    assert_eq!(run(src), "1 2 5 6\n");
}

#[test]
fn malloc_type_punning() {
    // A byte-addressable heap: an I64 buffer aliased through a U8* reads its
    // individual bytes (little-endian), matching the native heap. Also exercises
    // heap pointer arithmetic, which scales by the element size.
    let src = r#"
        U0 Main() {
            I64 *p = MAlloc(16);
            p[0] = 0x4142;                 // 'AB' little-endian
            U8 *b = p;                     // alias the same memory as bytes
            "%d %d\n", b[0], b[1];         // 0x42 0x41
            I64 *q = p + 1;                // pointer arithmetic (one I64 = 8 bytes)
            *q = 7;
            "%d %d\n", b[8], q - p;        // byte 8 is q[0]'s low byte; q - p == 1
        }
        Main;
    "#;
    assert_eq!(run(src), "66 65\n7 1\n");
}

#[test]
fn malloc_linked_list() {
    // A linked list built on the heap, exercising `->` through MAlloc'd nodes.
    let src = r#"
        class Node { I64 val; Node *next; }
        U0 Main() {
            Node *head = MAlloc(sizeof(Node));
            head->val = 1;
            head->next = MAlloc(sizeof(Node));
            head->next->val = 2;
            head->next->next = NULL;
            Node *n = head;
            while (n != NULL) { "%d ", n->val; n = n->next; }
            "\n";
        }
        Main;
    "#;
    assert_eq!(run(src), "1 2 \n");
}

#[test]
fn strlen_on_a_char_buffer() {
    // StrLen on an array argument walks to the NUL terminator.
    let src = r#"
        U0 Main() {
            U8 buf[8];
            buf[0] = 'h'; buf[1] = 'i'; buf[2] = '!'; buf[3] = 0;
            "%d\n", StrLen(buf);
        }
        Main;
    "#;
    assert_eq!(run(src), "3\n");
}

#[test]
fn pointer_arithmetic_indexing_and_comparison() {
    let src = r#"
        U0 Main() {
            I64 a[5];
            I64 *p = &a[0];
            *p = 10;
            *(p + 1) = 20;
            p[2] = 30;
            I64 *stop = &a[5];
            I64 sum = 0;
            I64 *q = &a[0];
            while (q < stop) {
                sum += *q;
                q++;
            }
            "%d %d %d\n", a[0], a[1], a[2];
            "diff=%d sum=%d\n", stop - &a[0], sum;
        }
        Main;
    "#;
    assert_eq!(run(src), "10 20 30\ndiff=5 sum=60\n");
}

#[test]
fn pointer_equality_with_null() {
    let src = r#"
        U0 Main() {
            I64 x = 7;
            I64 *p = NULL;
            if (p == NULL) "is null\n";
            p = &x;
            if (p != NULL) "now set: %d\n", *p;
        }
        Main;
    "#;
    assert_eq!(run(src), "is null\nnow set: 7\n");
}

#[test]
fn cast_truncates_to_width() {
    // (U8)300 -> 44, (I8)200 -> -56, (U16)70000 -> 4464
    assert_eq!(
        run(r#""%d %d %d\n", (U8)300, (I8)200, (U16)70000;"#),
        "44 -56 4464\n"
    );
}

#[test]
fn sizeof_variable_length_array_matches_allocation() {
    // The array dimension is a runtime value; sizeof agrees with what was
    // allocated (4 elements * 8 bytes).
    let src = r#"
        U0 Main() {
            I64 n = 4;
            I64 a[n];
            a[3] = 99;
            "%d %d\n", sizeof(a), a[3];
        }
        Main;
    "#;
    assert_eq!(run(src), "32 99\n");
}

#[test]
fn goto_to_label_in_enclosing_block() {
    // The label is at the function-body level; the goto fires from inside a
    // nested block and resumes at the enclosing label.
    let src = r#"
        U0 Main() {
            I64 i = 0;
        loop:
            {
                "%d ", i;
                i++;
                if (i < 3) goto loop;
            }
        }
        Main;
    "#;
    assert_eq!(run(src), "0 1 2 ");
}

// ---- error reporting ----

#[test]
fn division_by_zero_is_a_runtime_error() {
    let program = parse("I64 x = 1 / 0;").unwrap();
    let mut interp = Interpreter::new(Vec::<u8>::new());
    let err = interp.run(&program).unwrap_err();
    assert!(err.message.contains("division by zero"), "got: {err}");
}

#[test]
fn null_dereference_is_a_runtime_error() {
    let src = "U0 F() { I64 *p = NULL; *p = 1; } F;";
    let program = parse(src).unwrap();
    let mut interp = Interpreter::new(Vec::<u8>::new());
    let err = interp.run(&program).unwrap_err();
    assert!(err.message.contains("null pointer"), "got: {err}");
}

#[test]
fn backend_name() {
    let interp = Interpreter::new(Vec::<u8>::new());
    assert_eq!(interp.name(), "interp");
}

#[test]
fn run_to_string_reports_semantic_errors() {
    // run_to_string type-checks first, so a semantic error surfaces instead of a
    // confusing runtime fault.
    let program = parse("U0 Main() { Frobnicate(1); } Main;").unwrap();
    let err = run_to_string(&program).unwrap_err();
    assert!(err.message.contains("Frobnicate"), "got: {err}");
}
