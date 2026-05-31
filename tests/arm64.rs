//! Tests for the hand-rolled AArch64 (`arm64`) backend.
//!
//! Each test compiles a small HolyC program to a native executable, runs it, and
//! checks the process exit status equals the program's `return` value. These
//! only run where a C toolchain (`cc`) and an arm64-Darwin host are available;
//! on other platforms they skip (the backend targets aarch64-apple-darwin).

use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

use solomon::backend::Backend;
use solomon::backend::arm64::Arm64;
use solomon::backend::interp::run_to_string;
use solomon::parser::parse;
use solomon::sema::check_program;

mod common;

/// Whether this host can build + run native arm64 Mach-O binaries.
fn toolchain_available() -> bool {
    cfg!(all(target_arch = "aarch64", target_os = "macos"))
        && Command::new("cc")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
}

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// Compile `src`, run the resulting binary, and return its exit status.
fn build_and_run(src: &str) -> i32 {
    let program = parse(src).unwrap_or_else(|e| panic!("parse failed: {e}"));
    let errs = check_program(&program);
    assert!(errs.is_empty(), "semantic errors: {errs:?}");

    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let out = std::env::temp_dir().join(format!("solomon-arm64-test-{}-{id}", std::process::id()));

    let mut backend = Arm64::new(&out);
    backend
        .run(&program)
        .unwrap_or_else(|e| panic!("arm64 build failed: {e}"));

    let status = Command::new(&out)
        .status()
        .unwrap_or_else(|e| panic!("could not run produced binary: {e}"));
    let _ = std::fs::remove_file(&out);
    status.code().expect("process terminated by signal")
}

/// Compile `src`, run the resulting binary, and return its captured stdout.
fn build_and_capture(src: &str) -> String {
    let program = parse(src).unwrap_or_else(|e| panic!("parse failed: {e}"));
    let errs = check_program(&program);
    assert!(errs.is_empty(), "semantic errors: {errs:?}");

    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let out = std::env::temp_dir().join(format!("solomon-arm64-out-{}-{id}", std::process::id()));
    Arm64::new(&out)
        .run(&program)
        .unwrap_or_else(|e| panic!("arm64 build failed: {e}"));
    let output = Command::new(&out)
        .output()
        .unwrap_or_else(|e| panic!("could not run produced binary: {e}"));
    let _ = std::fs::remove_file(&out);
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Assert the native backend produces byte-for-byte the same stdout as the
/// interpreter (the conformance oracle) for `src` — the strongest check, since
/// it needs no hand-computed expected value.
fn assert_native_matches_interp(src: &str) {
    let program = parse(src).unwrap_or_else(|e| panic!("parse failed: {e}"));
    let errs = check_program(&program);
    assert!(errs.is_empty(), "semantic errors: {errs:?}");
    let interp = run_to_string(&program).unwrap_or_else(|e| panic!("interp error: {e}"));
    assert!(
        !interp.is_empty(),
        "test program produced no interpreter output (likely a parse/build error):\n{src}"
    );
    let native = build_and_capture(src);
    assert_eq!(native, interp, "native != interp for:\n{src}");
}

#[test]
fn compiles_integer_expressions_to_exit_code() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    // (input source, expected exit status). Exit codes are 8-bit, so all values
    // are kept in 0..=255.
    let cases: &[(&str, i32)] = &[
        ("return 0;", 0),
        ("return 42;", 42),
        ("return 2 + 3 * 4 - 5;", 9),
        ("return (1 + 2) * (3 + 4);", 21),
        ("return 100 / 7;", 14),
        ("return 17 % 5;", 2),
        ("return 1 << 5;", 32),
        ("return 65536 >> 9;", 128),
        ("return (12 & 10) | 1;", 9),
        ("return 7 ^ 3;", 4),
        ("return -(-42);", 42),
        ("return ~0 & 255;", 255),
        ("return 65540 % 256;", 4), // exercises a >16-bit immediate (MOVK)
        ("return 250 + 5;", 255),
    ];
    for (src, expected) in cases {
        assert_eq!(build_and_run(src), *expected, "for source: {src}");
    }
}

#[test]
fn compiles_locals_and_control_flow() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    let cases: &[(&str, i32)] = &[
        // locals + for + compound assignment
        (
            "I64 s = 0; I64 i; for (i = 1; i <= 10; i++) s += i; return s;",
            55,
        ),
        // while loop (factorial)
        (
            "I64 n = 5; I64 f = 1; while (n > 1) { f *= n; n--; } return f;",
            120,
        ),
        // do-while
        ("I64 i = 0; do { i++; } while (i < 5); return i;", 5),
        // if / else
        ("I64 x = 3; if (x > 5) return 1; else return 2;", 2),
        // ternary
        ("I64 x = 10; return x > 5 ? 42 : 0;", 42),
        // short-circuit && (right side would divide by zero if evaluated)
        ("I64 a = 0; return (a != 0 && 1 / a) ? 9 : 7;", 7),
        // short-circuit ||
        ("I64 a = 1; return (a == 1 || 1 / 0) ? 5 : 6;", 5),
        // comparisons as integer values
        ("return (3 < 5) + (5 < 3) + (2 == 2);", 2),
        // logical not
        ("I64 x = 0; return !x + !!5;", 2),
        // switch with a range case
        (
            "I64 v = 2; switch (v) { case 0: return 100; case 1 ... 3: return 33; default: return 1; } return 0;",
            33,
        ),
        // switch fall-through then break
        (
            "I64 v = 0; I64 r = 0; switch (v) { case 0: r += 1; case 1: r += 10; break; case 2: r += 100; } return r;",
            11,
        ),
        // switch default
        (
            "I64 v = 9; switch (v) { case 0: return 1; default: return 99; } return 0;",
            99,
        ),
        // nested loops
        (
            "I64 s = 0; I64 i; I64 j; for (i = 0; i < 3; i++) for (j = 0; j < 3; j++) s++; return s;",
            9,
        ),
        // break + continue
        (
            "I64 s = 0; I64 i; for (i = 0; i < 10; i++) { if (i == 5) break; if (i % 2 == 0) continue; s += i; } return s;",
            4,
        ),
        // goto loop at top level
        (
            "I64 i = 0; I64 s = 0; top: s += i; i++; if (i < 5) goto top; return s;",
            10,
        ),
        // pre/post increment
        (
            "I64 i = 5; I64 a = i++; I64 b = ++i; return a * 10 + b;",
            57,
        ),
        // block scoping: a shadow in an inner block doesn't leak
        ("I64 x = 1; { I64 x = 9; } return x;", 1),
    ];
    for (src, expected) in cases {
        assert_eq!(build_and_run(src), *expected, "for source: {src}");
    }
}

#[test]
fn native_right_shift_signedness_matches_interp() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    // `>>` must be arithmetic (ASRV) for a signed left operand and logical (LSRV)
    // for unsigned — byte-identical to the interpreter
    // (right_shift_is_signedness_directed in tests/interp.rs).
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
    assert_eq!(build_and_capture(src), "-4 -8 800000000000000 -16\n");
}

#[test]
fn native_division_signedness_matches_interp() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    // `/` and `%` must be SDIV for a signed left operand and UDIV for unsigned —
    // byte-identical to the interpreter (division_is_signedness_directed).
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
    assert_eq!(build_and_capture(src), "4000000000000000 1 -3 -2 3\n");
}

#[test]
fn native_live_range_sharing_matches_interp() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    // Disjoint locals share a register (Chain: x/a/b/c coalesce); loop-carried
    // locals (Rotate: a/b/sum live across the back-edge) must not. Byte-identical
    // to the interpreter either way.
    let src = r#"
        I64 Chain(I64 x) { I64 a = x + x; I64 b = a + a; I64 c = b + b; return c + c; }
        I64 Rotate(I64 n) {
            I64 a = 1, b = 2, sum = 0, i;
            for (i = 0; i < n; i++) { sum = sum + a; a = b; b = sum; }
            return a + b + sum;
        }
        U0 Main() { "%d %d\n", Chain(3), Rotate(6); }
        Main;
    "#;
    assert_eq!(build_and_capture(src), "48 47\n");
}

#[test]
fn native_register_pressure_spills_match_interp() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    // More simultaneously-live promotable locals than the pool holds (13 ints >
    // x19..x28, 9 doubles > d8..d15): linear scan promotes what fits and leaves
    // the rest in frame slots. The spilled-vs-promoted mix must still match.
    assert_native_matches_interp(
        r#"
        I64 Many(I64 n) {
            I64 a=n+1, b=n+2, c=n+3, d=n+4, e=n+5, f=n+6, g=n+7,
                h=n+8, i=n+9, j=n+10, k=n+11, l=n+12, m=n+13;
            I64 s = a+b+c+d+e+f+g+h+i+j+k+l+m;
            s += a*2 + b*2 + m*2;
            return s;
        }
        F64 Big(I64 n) {
            F64 a=1.0,b=2.0,c=3.0,d=4.0,e=5.0,f=6.0,g=7.0,h=8.0,j=9.0;
            F64 s = a+b+c+d+e+f+g+h+j;
            s += a*2.0 + j*2.0;
            return s + n;
        }
        U0 Main() { "%d %.1f\n", Many(0), Big(0); }
        Main;
    "#,
    );
}

#[test]
fn native_promoted_pointer_locals_match_interp() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    // A pointer local promoted to a register, reassigned and dereferenced across a
    // loop: a linked-list walk (`p = p->next`) and a strided array walk (`p += 1`).
    assert_native_matches_interp(
        r#"
        class N { I64 v; N *next; }
        I64 Walk(N *head) {
            I64 sum = 0;
            N *p = head;
            while (p != NULL) { sum += p->v; p = p->next; }
            return sum;
        }
        I64 ArrSum(I64 *a, I64 n) {
            I64 *p = a, *stop = a + n, s = 0;
            while (p < stop) { s += *p; p += 1; }
            return s;
        }
        U0 Main() {
            N a, b, c;
            a.v = 10; a.next = &b; b.v = 20; b.next = &c; c.v = 30; c.next = NULL;
            I64 xs[5], i;
            for (i = 0; i < 5; i++) xs[i] = i * 10;
            "%d %d\n", Walk(&a), ArrSum(xs, 5);
        }
        Main;
    "#,
    );
}

#[test]
fn native_promoted_compound_assignment_matches_interp() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    // Every compound-assignment operator applied to a register-promoted local,
    // signed and unsigned (the unsigned path uses UDIV/LSRV) — the register
    // arithmetic must agree with the interpreter, including the final narrowing.
    assert_native_matches_interp(
        r#"
        I64 Sgn(I64 x) {
            I64 a = x;
            a += 5; a -= 2; a *= 3; a /= 2; a %= 7;
            a &= 0xF; a |= 0x10; a ^= 0x3; a <<= 1; a >>= 1;
            return a;
        }
        U64 Uns(U64 x) { x >>= 2; x /= 3; x %= 1000; return x; }
        U0 Main() {
            "%d %d\n", Sgn(10), Sgn(100);
            "%u %u\n", Uns(0xFFFFFFFFFFFFFFFF), Uns(123456);
        }
        Main;
    "#,
    );
}

#[test]
fn native_promoted_locals_survive_calls_match_interp() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    // Promoted locals are callee-saved, so they survive an intervening call:
    // recursion (a/b/c live across `Rec(n-1)`), a libc builtin call (`acc` across
    // `Sqrt`), and a function mixing both register pools (int `isum`, F64 `acc`).
    assert_native_matches_interp(
        r#"
        I64 Rec(I64 n) {
            if (n <= 0) return 0;
            I64 a = n, b = n - 1, c = n - 2;
            I64 sub = Rec(n - 1);
            return a + b + c + sub;
        }
        I64 Roots(I64 n) {
            I64 acc = 0, i;
            for (i = 1; i <= n; i++) { F64 r = Sqrt(i * i * 1.0); acc += (I64)r; }
            return acc;
        }
        F64 Mix(I64 n) {
            I64 i, isum = 0;
            F64 acc = 0.0;
            for (i = 0; i < n; i++) { acc += 1.5; isum += i; }
            return acc + isum;
        }
        U0 Main() { "%d %d %.1f\n", Rec(4), Roots(4), Mix(4); }
        Main;
    "#,
    );
}

#[test]
fn native_goto_disables_sharing_matches_interp() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    // A function with `goto`/labels has unstructured control flow, so the allocator
    // widens every interval to the whole function (nothing shares) but still
    // promotes — the conservative fallback must stay correct.
    assert_native_matches_interp(
        r#"
        I64 Loop(I64 n) {
            I64 sum = 0, i = 0;
            top:
            if (i >= n) goto done;
            sum += i;
            i++;
            goto top;
            done:
            return sum;
        }
        U0 Main() { "%d %d\n", Loop(5), Loop(10); }
        Main;
    "#,
    );
}

#[test]
fn native_nested_ternary_and_deep_expr_match_interp() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    // Nested ternaries, a deeply nested arithmetic tree (exercising the operand
    // push/pop stack), and a multi-character constant packed little-endian.
    assert_native_matches_interp(
        r#"
        I64 T(I64 x) { return x > 10 ? (x > 20 ? 3 : 2) : (x > 0 ? 1 : 0); }
        U0 Main() {
            I64 i;
            for (i = -5; i <= 25; i += 5) "%d ", T(i);
            "\n";
            "%d\n", ((((1+2)*(3+4)) - ((5+6)*(7-8))) + (((9*2)+3) - (4*5)));
            I64 c = 'AB';
            "%d %c%c\n", c, c & 0xFF, (c >> 8) & 0xFF;
        }
        Main;
    "#,
    );
}

#[test]
fn native_uninitialized_aggregates_zero_filled_match_interp() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    // A local aggregate without an initializer is zero-filled (gen_zero_slot) so
    // its untouched elements/fields read as 0, matching the interpreter — covers a
    // plain array, a class, an array of classes, a class with an array field, and
    // a union. (Scalars were already zeroed; aggregates were stack garbage.)
    assert_native_matches_interp(
        r#"
        class P { I64 x; I64 y; }
        class B { I64 t; I64 data[3]; }
        union U { I64 w; U8 b[8]; }
        U0 Main() {
            I64 arr[4]; arr[1] = 5;
            P p; p.y = 9;
            P ps[2]; ps[1].x = 7;
            B b; b.data[2] = 3;
            U u;
            "%d %d %d %d %d %d %d %d\n",
                arr[0] + arr[1], arr[3], p.x + p.y, ps[0].x, ps[1].x,
                b.data[0] + b.data[2], b.t, u.w;
        }
        Main;
    "#,
    );
}

#[test]
fn native_pointer_increment_matches_interp() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    // Pointer `++`/`--` (element-scaled) in its various forms, including a
    // string-literal walk-and-subtract and pre/post `*p++` — all must agree with
    // the interpreter (which the matching tests/interp.rs cases pin down).
    assert_native_matches_interp(
        r#"
        I64 Walk(U8 *s) { U8 *p = s; while (*p) p++; return p - s; }
        I64 ArrSum(I64 *a, I64 n) {
            I64 *p = a, s = 0, i;
            for (i = 0; i < n; i++) { s += *p; p++; }
            return s;
        }
        U0 Main() {
            I64 xs[5], i;
            for (i = 0; i < 5; i++) xs[i] = i * 7;
            I64 *p = &xs[4]; p--; p--;
            U8 *src = "world"; U8 *dst = MAlloc(8); U8 *d = dst;
            while (*src) *d++ = *src++;
            *d = 0;
            "%d %d %d %s\n", Walk("hello"), ArrSum(xs, 5), *p, dst;
        }
        Main;
    "#,
    );
}

#[test]
fn native_lvalue_increment_and_compound_matches_interp() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    // `++`/compound assignment whose target is *not* a plain local: an array
    // element, a struct field by value and through a pointer, a dereferenced
    // pointer, and a global — each writes back through the right address.
    assert_native_matches_interp(
        r#"
        class P { I64 x; I64 y; }
        I64 g = 100;
        U0 Main() {
            I64 a[3]; a[0] = 10; a[1] = 0; a[2] = 3;
            a[0]++; ++a[1]; a[2] *= 4;
            P p; p.x = 5; p.x++;
            P *pp = &p; pp->y = 1; pp->y++;
            I64 v = 10; I64 *ptr = &v; *ptr += 5;
            g--;
            "%d %d %d %d %d %d %d\n", a[0], a[1], p.x, pp->y, v, a[2], g;
        }
        Main;
    "#,
    );
}

#[test]
fn native_shift_edges_and_cast_chains_match_interp() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    // Shift amounts at the edges (0, 62, 63) and chained casts that truncate then
    // widen (I64 -> U8 -> I64; a 32-bit all-ones constant into I32 sign-extends).
    assert_native_matches_interp(
        r#"
        U0 Main() {
            I64 x = 1;
            U64 y = 0x8000000000000000;
            "%d %d %u %u\n", x << 0, x << 62, y >> 63, y >> 0;
            I64 a = 300;
            U8 b = (U8)a;
            I64 c = (I64)b;
            I32 d = (I32)0xFFFFFFFF;
            "%d %d %d\n", b, c, d;
        }
        Main;
    "#,
    );
}

#[test]
fn native_strength_reduction_matches_interp() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    // `* / %` by a constant power of two strength-reduce to `lsl` / (unsigned)
    // `lsr` / `and #2^k-1`. Signed `*` by a power of two is still a shift (wraps
    // mod 2^64), but signed `/` / `%` and any non-power-of-two keep the generic
    // SDIV/UDIV/MUL path. All must stay byte-identical to the interpreter.
    assert_native_matches_interp(
        r#"
        U64 Uns(U64 x) { return x * 8 + x / 4 + x % 16 + x % 1; }
        I64 Sgn(I64 x) { return x * 4 + (-x) * 2; }
        I64 Gen(I64 x) { return x * 3 + x / 5 + x % 7 + (-9) / 2; }
        U0 Main() { "%u %d %d\n", Uns(1000), Sgn(-7), Gen(100); }
        Main;
    "#,
    );
}

#[test]
fn native_register_spill_prefers_hot_vars_match_interp() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    // Twelve cold locals plus a hot loop accumulator/counter compete for ten
    // integer registers. The linear-scan spill heuristic evicts the coldest active
    // interval for a hotter one, so the loop variables win registers and the cold
    // ones fall back to slots — and the result still matches the interpreter.
    assert_native_matches_interp(
        r#"
        I64 F(I64 n) {
            I64 a=n+1, b=n+2, c=n+3, d=n+4, e=n+5, f=n+6,
                g=n+7, h=n+8, j=n+9, k=n+10, l=n+11, m=n+12;
            I64 acc = 0, i;
            for (i = 0; i < n; i++) acc += i * 2;
            return acc + a+b+c+d+e+f+g+h+j+k+l+m;
        }
        U0 Main() { "%d %d\n", F(8), F(20); }
        Main;
    "#,
    );
}

#[test]
fn native_f64_mixed_operand_arithmetic_matches_interp() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    // Guards the F64 simple-operand elision (`is_simple_foperand`): a binary op
    // whose rhs is a literal or scalar keeps its lhs in FT2 with an `fmov` instead
    // of the GPR/stack round-trip, while a complex rhs still spills. Mixed simple
    // and complex float operands must stay byte-identical to the interpreter.
    assert_native_matches_interp(
        r#"
        F64 Calc(F64 a, F64 b) {
            F64 c = a * b + 2.0 - a;     // simple operands (scalars + a literal)
            F64 d = (a + b) * (a - b);   // complex operands (parenthesized binaries)
            return c + d / a;
        }
        U0 Main() { "%.4f %.4f\n", Calc(3.0, 4.0), Calc(1.5, 0.5); }
        Main;
    "#,
    );
}

#[test]
fn native_f64_register_promotion_matches_interp() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    // F64 promotion to callee-saved d8..d15: a loop float accumulator whose value
    // survives a call to a function that also uses d8 (so it must save/restore).
    let src = r#"
        F64 Scale(F64 x) { F64 r = x + x; return r * r; }
        F64 Run(I64 n) {
            F64 acc = 0.0, step = 1.5;
            I64 i;
            for (i = 0; i < n; i++) acc += Scale(step);
            return acc;
        }
        U0 Main() { "%.2f\n", Run(3); }
        Main;
    "#;
    assert_eq!(build_and_capture(src), "27.00\n");
}

#[test]
fn native_register_promotion_matches_interp() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    // Register promotion: loop accumulator/counter, recursion (promoted locals
    // survive recursive calls via the callee-saved ABI), and narrow-width wrap.
    let src = r#"
        U8 Narrow(U8 x) { x += 200; return x; }
        I64 Sum(I64 n) { I64 acc = 0, i; for (i = 1; i <= n; i++) acc += i; return acc; }
        I64 Fib(I64 n) { I64 a, b; if (n < 2) return n; a = Fib(n - 1); b = Fib(n - 2); return a + b; }
        U0 Main() { "%d %d %d\n", Sum(10), Fib(10), Narrow(100); }
        Main;
    "#;
    assert_eq!(build_and_capture(src), "55 55 44\n");
}

#[test]
fn native_nested_calls_match_interp() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    // Exercises the peephole's mov fusion (argument-setup + return moves through
    // x9/x10) — byte-identical to the interpreter.
    let src = r#"
        I64 Add(I64 a, I64 b) { return a + b; }
        I64 Mul(I64 a, I64 b) { return a * b; }
        U0 Main() { "%d\n", Add(Mul(3, 4), Add(5, 6)); }
        Main;
    "#;
    assert_eq!(build_and_capture(src), "23\n");
}

#[test]
fn native_mixed_operand_arithmetic_matches_interp() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    // Guards the codegen optimizations (constant folding + the simple-operand
    // push/pop elision) against behavior change — byte-identical to the interp
    // (mixed_operand_arithmetic in tests/interp.rs).
    let src = r#"
        U0 Main() {
            I64 a = 10, b = 3;
            "%d %d %d %d\n", a + 2 * 3, a * a + b, (a + b) * (a - b), a % 7 - (1 << 2);
        }
        Main;
    "#;
    assert_eq!(build_and_capture(src), "16 103 91 -1\n");
}

#[test]
fn native_chained_comparisons_match_interp() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    // HolyC chained range comparisons (`a < b < c` = `a<b && b<c`) — byte-identical
    // to the interpreter (chained_range_comparisons in tests/interp.rs).
    let src = r#"
        U0 Main() {
            "%d %d %d %d\n", 2 < 3 < 5, 2 < 2 < 5, 1 < 2 < 3 < 4, 5 < 4 < 3;
            I64 a = 5;
            "%d %d\n", (a < 10) < 2, a < 10 < 2;
        }
        Main;
    "#;
    assert_eq!(build_and_capture(src), "1 0 1 0\n1 0\n");
}

#[test]
fn native_comparison_signedness_matches_interp() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    // Relational compares use unsigned conditions (LO/HI/LS/HS) when either
    // operand is unsigned, and integer compares are full-width — byte-identical
    // to the interpreter (comparison_is_signedness_directed_and_exact).
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
    assert_eq!(build_and_capture(src), "1 0 0 1 0 1\n");
}

#[test]
fn native_narrow_integer_wrapping_matches_interp() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    // Narrow integers truncate to width on store, argument passing, and return
    // (mid-expression stays I64) — byte-identical to the interpreter
    // (narrow_integer_types_wrap_on_store_arg_and_return).
    let src = r#"
        U8 AddU8(U8 a, U8 b) { return a + b; }
        I8 AddI8(I8 a, I8 b) { return a + b; }
        U0 Main() {
            U8 x = 200; x = x + 100;
            I8 s = 100; s = s + 100;
            U8 a = 200; I64 wide = a + 100;
            U8 c = 250; c += 10;
            "%d %d %d %d %d %d\n",
                x, s, wide, AddU8(300, 0), AddU8(200, 100), AddI8(100, 100);
        }
        Main;
    "#;
    assert_eq!(build_and_capture(src), "44 -56 300 44 44 -56\n");
}

#[test]
fn native_printf_flags_width_precision_matches_interp() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    // Flags/width/precision/octal/length lower to libc with the `ll` length
    // injected — byte-identical to the interpreter
    // (printf_flags_width_precision_octal in tests/interp.rs).
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
        build_and_capture(src),
        "[   42][42   ][000ff][+7][0xff][005][100]\n\
         [     hello][he        ][A]\n\
         [ffffffffffffffff][18446744073709551615]\n"
    );
}

#[test]
fn native_memsearch_and_number_to_str_match_interp() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    // MemSearch -> memmem; I64ToStr/F64ToStr -> sprintf with a fixed format —
    // byte-identical to the interpreter (memsearch_and_number_to_str).
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
    assert_eq!(build_and_capture(src), "4 1\n-12345\n3.14\n1e+06\n");
}

#[test]
fn native_strspn_strcspn_match_interp() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    // StrSpn -> strspn, StrCSpn -> strcspn — byte-identical to the interpreter
    // (strspn_and_strcspn in tests/interp.rs).
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
    assert_eq!(build_and_capture(src), "3 3 0 3\n");
}

#[test]
fn native_strchr_strlastchr_match_interp() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    // StrChr -> strchr, StrLastChr -> strrchr — byte-identical to the interpreter
    // (strchr_and_strlastchr in tests/interp.rs).
    let src = r#"
        U0 Main() {
            U8 *p = MAlloc(32); StrCpy(p, "/usr/local/bin");
            "%d %d %s\n", StrChr(p, '/') - p, StrLastChr(p, '/') - p, StrLastChr(p, '/') + 1;
            "%d %d\n", StrChr(p, 'Z') == NULL, StrChr(p, 0) - p;
            Free(p);
        }
        Main;
    "#;
    assert_eq!(build_and_capture(src), "0 10 bin\n1 14\n");
}

#[test]
fn native_strrev_and_memfind_match_interp() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    // StrRev (inline two-pointer loop) and MemFind (-> memchr) — byte-identical
    // to the interpreter (strrev_and_memfind in tests/interp.rs).
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
    assert_eq!(build_and_capture(src), "olleH\nx\n2 1\n");
}

#[test]
fn native_str_case_str2f64_memmove_match_interp() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    // StrToF64 -> atof, MemMove -> memmove, StrToUpper/StrToLower (inline loop
    // over toupper/tolower) — byte-identical to the interpreter
    // (str_case_str2f64_and_memmove in tests/interp.rs).
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
        build_and_capture(src),
        "3.140 -250.000 6.000\nHELLO, WORLD 42!\nhello, world 42!\nababcd ret=1\n"
    );
}

#[test]
fn native_mstrprint_and_str2i64_match_interp() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    // MStrPrint lowers to snprintf(measure) + malloc + sprintf; StrToI64 -> atoll.
    // Byte-identical to the interpreter (mstrprint_and_str2i64 in tests/interp.rs).
    let src = r#"
        U0 Main() {
            U8 *s = MStrPrint("n=%d hex=%x pi=%.2f", 42, 255, 3.14159);
            "%s len=%d\n", s, StrLen(s);
            Free(s);
            "%d %d %d %d\n", StrToI64("123"), StrToI64("-45"), StrToI64("  7x"), StrToI64("abc");
        }
        Main;
    "#;
    assert_eq!(
        build_and_capture(src),
        "n=42 hex=ff pi=3.14 len=19\n123 -45 7 0\n"
    );
}

#[test]
fn native_catprint_matches_interp() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    // `CatPrint` appends via `sprintf(dst + strlen(dst), ...)` — byte-identical
    // to the interpreter (catprint_appends_to_a_buffer in tests/interp.rs).
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
        build_and_capture(src),
        "Items: #1=1 #2=4 #3=9 | total=14.0\nret_eq=1 len=34\n"
    );
}

#[test]
fn native_strprint_matches_interp() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    // `StrPrint` lowers to `sprintf` (returning dst) — byte-identical to the
    // interpreter (strprint_formats_into_a_buffer in tests/interp.rs).
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
        build_and_capture(src),
        "x=42 hex=000ff pi=3.14 e=1.500e+00 s=hi ret_eq=1\n0.0001/1e+06\n"
    );
}

#[test]
fn native_scientific_general_floats_match_interp() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    // `%e`/`%E`/`%g`/`%G` lower to libc — byte-identical to the interpreter's
    // own rendering (printf_scientific_and_general_floats in tests/interp.rs).
    let src = r#"
        U0 Main() {
            "[%e][%E][%.2e]\n", 1.5, 1234.5, 9.9999996;
            "[%g][%g][%g][%.3g][%#g]\n", 1.5, 1000000.0, 0.0001, 1234567.0, 1.5;
            "[%12.3e][%-12.3e][%+g]\n", 1.5, 1.5, 2.5;
        }
        Main;
    "#;
    assert_eq!(
        build_and_capture(src),
        "[1.500000e+00][1.234500E+03][1.00e+01]\n\
         [1.5][1e+06][0.0001][1.23e+06][1.50000]\n\
         [   1.500e+00][1.500e+00   ][+2.5]\n"
    );
}

#[test]
fn native_float_to_unsigned_matches_interp() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    // Float -> unsigned uses FCVTZU (init/cast/assign/return); signed uses FCVTZS
    // (saturates); a negative float clamps to 0 — byte-identical to the interp
    // (float_to_unsigned_uses_unsigned_conversion in tests/interp.rs).
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
        build_and_capture(src),
        "10000000000000000000 10000000000000000000 9223372036854775807 \
         15000000000000000000 0 12000000000000000000\n"
    );
}

#[test]
fn native_switch_start_end_matches_interp() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    // The bracketed `switch [n]` with `start:` prologue and `end:` epilogue must
    // produce the same result as the interpreter (see tests/interp.rs).
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
    assert_eq!(build_and_capture(src), "101 1102 1100\n");
}

#[test]
fn native_switch_jump_table_matches_interp() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    // A dense switch lowers to an O(1) branch table; the result must match the
    // interpreter (see switch_dense_cases_with_gaps_and_range in tests/interp.rs)
    // across a gap (3), a range (5..7), and out-of-range values -> default.
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
    assert_eq!(
        build_and_capture(src),
        "99 10 11 12 99 14 57 57 57 99 99 \n"
    );
}

#[test]
fn native_switch_const_folded_cases_match_interp() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    // Case labels are constant arithmetic the backend folds (const_eval_i64) to
    // dense values 0..5, so this lowers to the jump table; the result must match
    // the interpreter (switch_with_constant_folded_case_values in tests/interp.rs).
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
    assert_eq!(build_and_capture(src), "-1 10 11 12 13 14 15 -1 \n");
}

#[test]
fn compiles_functions_and_calls() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    let cases: &[(&str, i32)] = &[
        // simple call with two args
        (
            "I64 Add(I64 a, I64 b) { return a + b; } return Add(40, 2);",
            42,
        ),
        // recursion
        (
            "I64 Fib(I64 n) { if (n < 2) return n; return Fib(n - 1) + Fib(n - 2); } return Fib(10);",
            55,
        ),
        (
            "I64 F(I64 n) { if (n <= 1) return 1; return n * F(n - 1); } return F(5);",
            120,
        ),
        // default arguments (omitted and supplied)
        (
            "I64 G(I64 a, I64 b = 10) { return a + b; } return G(5);",
            15,
        ),
        (
            "I64 G(I64 a, I64 b = 10) { return a + b; } return G(5, 2);",
            7,
        ),
        // nested calls
        (
            "I64 Sq(I64 x) { return x * x; } I64 Sum(I64 a, I64 b) { return a + b; } return Sum(Sq(3), Sq(4));",
            25,
        ),
        // an earlier argument must survive a call made while evaluating a later one
        (
            "I64 Id(I64 x) { return x; } I64 Add(I64 a, I64 b) { return a + b; } return Add(Id(40), Id(2));",
            42,
        ),
        // mutual recursion with a forward reference
        (
            "I64 IsEven(I64 n) { if (n == 0) return 1; return IsOdd(n - 1); } \
          I64 IsOdd(I64 n) { if (n == 0) return 0; return IsEven(n - 1); } return IsEven(10);",
            1,
        ),
        // locals + a loop inside a function
        (
            "I64 F(I64 n) { I64 s = 0; I64 i; for (i = 1; i <= n; i++) s += i; return s; } return F(10);",
            55,
        ),
        // a bare function-name statement is a call
        ("U0 P() { } P; return 5;", 5),
        // a call used within a larger expression
        ("I64 V() { return 7; } return V() + V() * 2;", 21),
        // up to 8 integer arguments
        (
            "I64 S(I64 a,I64 b,I64 c,I64 d,I64 e,I64 f,I64 g,I64 h){return a+b+c+d+e+f+g+h;} return S(1,2,3,4,5,6,7,8);",
            36,
        ),
    ];
    for (src, expected) in cases {
        assert_eq!(build_and_run(src), *expected, "for source: {src}");
    }
}

#[test]
fn compiles_printing() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    let cases: &[(&str, &str)] = &[
        // implicit print of a bare string
        (r#""Hello, World!\n";"#, "Hello, World!\n"),
        // formatted integers (HolyC %d -> C %lld)
        (r#"I64 x = 42; "x=%d\n", x;"#, "x=42\n"),
        (r#""%d %d %d\n", 1, 2, 3;"#, "1 2 3\n"),
        // hex and char
        (r#""%x\n", 255;"#, "ff\n"),
        (r#""%c%c\n", 65, 66;"#, "AB\n"),
        // literal percent
        (r#""100%%\n";"#, "100%\n"),
        // a string-literal argument
        (r#""%s!\n", "hi";"#, "hi!\n"),
        // the Print() builtin
        (r#"Print("n=%d\n", 7);"#, "n=7\n"),
        // print inside a function, called from the top level
        (r#"U0 Greet(I64 n) { "hi %d\n", n; } Greet(5);"#, "hi 5\n"),
        // compute + print in a loop (printf reloc + recursion + control flow)
        (
            "I64 Fib(I64 n){ if(n<2) return n; return Fib(n-1)+Fib(n-2); } \
             I64 i; for(i=0;i<10;i++) Print(\"%d \", Fib(i)); \"\\n\";",
            "0 1 1 2 3 5 8 13 21 34 \n",
        ),
        // an argument whose evaluation itself calls printf-free code
        ("I64 Sq(I64 x){return x*x;} \"%d\\n\", Sq(9);", "81\n"),
    ];
    for (src, expected) in cases {
        assert_eq!(build_and_capture(src), *expected, "for source: {src}");
    }
}

#[test]
fn compiles_pointers_arrays_structs() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    let cases: &[(&str, i32)] = &[
        // pointer deref write-through
        ("I64 x = 5; I64 *p = &x; *p = 7; return x;", 7),
        // pointer arithmetic (scaled by element size)
        (
            "I64 a[4]; a[0]=10; a[1]=20; a[2]=30; a[3]=40; I64 *p=&a[0]; return *(p+2);",
            30,
        ),
        // array indexing in a loop
        (
            "I64 a[5]; I64 i; for(i=0;i<5;i++) a[i]=i*i; I64 s=0; for(i=0;i<5;i++) s+=a[i]; return s;",
            30,
        ),
        // struct field access via value and pointer
        (
            "class P{I64 x; I64 y;} P p; p.x=3; p.y=4; return p.x + p.y;",
            7,
        ),
        (
            "class P{I64 x; I64 y;} P p; P *pp=&p; pp->x=10; pp->y=20; return pp->x+pp->y;",
            30,
        ),
        // mixed-width fields laid out by the layout pass; writes don't clobber neighbours
        (
            "class M{ U8 a; I32 b; U8 c; } M m; m.a=3; m.b=70000; m.c=5; return m.a*10 + m.c;",
            35,
        ),
        ("class M{ U8 a; I32 b; U8 c; } return sizeof(M);", 12),
        // narrow array element widths
        (
            "I8 b[3]; b[0]=10; b[1]=20; b[2]=30; return b[0]+b[1]+b[2];",
            60,
        ),
        // narrow local truncates on store
        ("U8 x; x = 300; return x;", 44),
        // pointer difference is in elements
        ("I64 a[10]; I64 *p=&a[3]; I64 *q=&a[7]; return q-p;", 4),
        // sizeof a class
        ("class P{I64 x; I64 y;} return sizeof(P);", 16),
        // 2-D array indexing
        (
            "I64 m[2][2]; m[0][0]=1; m[0][1]=2; m[1][0]=3; m[1][1]=4; \
          return m[0][0]+m[0][1]+m[1][0]+m[1][1];",
            10,
        ),
        // a linked list over an array pool (pointers into an array of structs)
        (
            "class N{I64 v; N *next;} N pool[2]; \
          pool[0].v=10; pool[0].next=&pool[1]; pool[1].v=20; pool[1].next=NULL; \
          N *p=&pool[0]; I64 s=0; while(p!=NULL){ s+=p->v; p=p->next; } return s;",
            30,
        ),
        // a function taking a pointer parameter (out-param)
        (
            "U0 SetTo(I64 *p, I64 v){ *p = v; } I64 x; SetTo(&x, 99); return x;",
            99,
        ),
    ];
    for (src, expected) in cases {
        assert_eq!(build_and_run(src), *expected, "for source: {src}");
    }
}

#[test]
fn compiles_offset() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    let cases: &[(&str, i32)] = &[
        // a simple member offset (folded to a compile-time constant)
        ("class P{I64 a; I64 b;} return offset(P.b);", 8),
        // padded layout: U8 then I32 lands at offset 4
        ("class M{U8 a; I32 b;} return offset(M.b);", 4),
        // nested member path
        (
            "class Pt{I64 x; I64 y;} class Box{Pt lo; Pt hi;} return offset(Box.hi.y);",
            24,
        ),
        // offset is a plain I64 value, usable in arithmetic
        (
            "class P{I64 a; I64 b; I64 c;} return offset(P.c) - offset(P.b);",
            8,
        ),
    ];
    for (src, expected) in cases {
        assert_eq!(build_and_run(src), *expected, "for source: {src}");
    }
}

#[test]
fn compiles_aggregate_initializers() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    let cases: &[(&str, i32)] = &[
        ("I64 a[] = {10, 20, 30}; return a[0] + a[1] + a[2];", 60),
        ("I64 a[5] = {1, 2}; return a[2] + a[3] + a[4];", 0),
        (
            "I64 a[5] = {1, 2, 7}; return a[0] + a[1] + a[2] + a[3];",
            10,
        ),
        (
            "class P{I64 x; I64 y;} P p = {7, 8}; return p.x * 10 + p.y;",
            78,
        ),
        (
            "I64 m[2][2] = {{1, 2}, {3, 4}}; return m[0][0] + m[0][1] + m[1][0] + m[1][1];",
            10,
        ),
        (
            "class P{I64 x; I64 y;} P ps[2] = {{1, 2}, {3, 4}}; return ps[0].y + ps[1].x;",
            5,
        ),
        ("U8 b[3] = {10, 20, 30}; return b[0] + b[1] + b[2];", 60),
        (
            "I64 g[4] = {2, 4}; I64 Sum(){ return g[0] + g[1] + g[2] + g[3]; } return Sum();",
            6,
        ),
        (
            "class P{I64 x; I64 y;} P gp = {5, 9}; I64 G(){ return gp.x + gp.y; } return G();",
            14,
        ),
    ];
    for (src, expected) in cases {
        assert_eq!(build_and_run(src), *expected, "for source: {src}");
    }
}

#[test]
fn compiles_designated_initializers() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    let cases: &[(&str, i32)] = &[
        (
            "class P{I64 x; I64 y;} P p = {.x = 7, .y = 8}; return p.x * 10 + p.y;",
            78,
        ),
        // Out of order.
        (
            "class P{I64 x; I64 y;} P p = {.y = 8, .x = 7}; return p.x * 10 + p.y;",
            78,
        ),
        // Partial: the unset field stays zero.
        (
            "class P{I64 x; I64 y;} P p = {.y = 9}; return p.x * 10 + p.y;",
            9,
        ),
        // Nested designated, with the first field left default.
        (
            "class P{I64 x; I64 y;} class L{P a; P b; I64 t;} \
             L l = {.t = 5, .b = {.x = 2, .y = 3}}; \
             return l.a.x + l.b.x * 10 + l.b.y + l.t;",
            28,
        ),
        // Global designated initializer read from a function.
        (
            "class P{I64 x; I64 y;} P gp = {.y = 9, .x = 5}; \
             I64 G(){ return gp.x + gp.y; } return G();",
            14,
        ),
    ];
    for (src, expected) in cases {
        assert_eq!(build_and_run(src), *expected, "for source: {src}");
    }
}

#[test]
fn compiles_initializer_element_types() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    let cases: &[(&str, i32)] = &[
        // Char literals into a U8 array, plus the zero-filled tail. Kept under
        // 256 (exit codes are 8-bit): (97-90)+(98-90)+(99-90)+0 = 24.
        (
            "U8 cs[4] = {'a', 'b', 'c'}; return cs[0]-90 + cs[1]-90 + cs[2]-90 + cs[3];",
            24,
        ),
        // Constant-expression values and a trailing comma.
        ("I64 e[2] = {1 + 2, 3 * 4,}; return e[0] + e[1];", 15),
        // F64 array initializer, read back through truncation to int.
        ("F64 fs[3] = {1.5, 2.5}; return fs[0] + fs[1] + fs[2];", 4),
        // Designated F64 field.
        (
            "class V{F64 x; F64 y;} V v = {.y = 4.5}; return v.x + v.y;",
            4,
        ),
        // Array of designated struct initializers.
        (
            "class P{I64 x; I64 y;} P ps[2] = {{.x = 1, .y = 2}, {.y = 4}}; \
             return ps[0].x + ps[0].y + ps[1].x + ps[1].y;",
            7,
        ),
    ];
    for (src, expected) in cases {
        assert_eq!(build_and_run(src), *expected, "for source: {src}");
    }
}

#[test]
fn empty_program_exits_zero() {
    if !toolchain_available() {
        return;
    }
    assert_eq!(build_and_run(";"), 0);
}

#[test]
fn produces_a_native_binary() {
    if !toolchain_available() {
        return;
    }
    let program = parse("return 1;").unwrap();
    let out = std::env::temp_dir().join(format!("solomon-native-file-{}", std::process::id()));
    Arm64::new(&out).run(&program).unwrap();
    // It's a real Mach-O arm64 executable.
    let header = std::fs::read(&out).unwrap();
    let _ = std::fs::remove_file(&out);
    assert_eq!(&header[0..4], &[0xCF, 0xFA, 0xED, 0xFE]); // MH_MAGIC_64, little-endian
}

#[test]
fn compiles_globals() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    let cases: &[(&str, i32)] = &[
        // a global read from a function
        ("I64 g; I64 F(){ return g; } g = 42; return F();", 42),
        // a global mutated through a function call
        (
            "I64 counter; U0 Inc(){ counter++; } Inc(); Inc(); Inc(); return counter;",
            3,
        ),
        // global with an initialiser
        ("I64 g = 100; return g + 5;", 105),
        // global array
        (
            "I64 arr[3]; arr[0]=1; arr[1]=2; arr[2]=3; I64 s=0; I64 i; for(i=0;i<3;i++) s+=arr[i]; return s;",
            6,
        ),
        // global struct
        (
            "class P{I64 x; I64 y;} P p; p.x=3; p.y=4; return p.x+p.y;",
            7,
        ),
        // a linked list over a *global* array pool, walked through pointers
        (
            "class N{I64 v; N *next;} N pool[3]; \
          pool[0].v=1; pool[0].next=&pool[1]; pool[1].v=2; pool[1].next=&pool[2]; \
          pool[2].v=3; pool[2].next=NULL; \
          N *p=&pool[0]; I64 s=0; while(p!=NULL){ s+=p->v; p=p->next; } return s;",
            6,
        ),
    ];
    for (src, expected) in cases {
        assert_eq!(build_and_run(src), *expected, "for source: {src}");
    }
}

/// The whole `linklist.hc` sample (globals + pointers + sorted insertion)
/// compiles natively and prints exactly what the interpreter does.
#[test]
fn compiles_linklist_sample() {
    if !toolchain_available() {
        return;
    }
    let out = build_and_capture(include_str!("../examples/linklist.hc"));
    assert_eq!(out, "sorted: 1 2 3 5 7 8 9 \nlength=7 gcd(48,36)=12\n");
}

#[test]
fn compiles_floats() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    // Results are cast to integers so they can be returned as exit codes.
    let cases: &[(&str, i32)] = &[
        // arithmetic, then truncate
        ("F64 x = 3.0; F64 y = 4.0; return (I64)(x*x + y*y);", 25),
        ("F64 x = 2.5; return (I64)(x + x);", 5),
        // division (3.5 truncates toward zero)
        ("F64 x = 7.0; return (I64)(x / 2.0);", 3),
        // unary negation
        ("F64 x = 5.0; return (I64)(-x + 8.0);", 3),
        // mixed int/float arithmetic promotes to F64
        ("return (I64)(3 + 1.5);", 4),
        // a bare float return from an int function is truncated
        ("return 9.99;", 9),
        // float comparison feeding a ternary
        ("F64 a = 1.5; return a < 2.0 ? 7 : 8;", 7),
        ("F64 a = 5.0; F64 b = 5.0; return a == b ? 1 : 0;", 1),
        // float in a boolean context (nonzero is true)
        ("F64 a = 0.0; return a ? 1 : 2;", 2),
        ("F64 a = 0.5; return a && 1 ? 3 : 4;", 3),
        // compound assignment
        ("F64 x = 2.0; x += 3.0; x *= 2.0; return (I64)x;", 10),
        // F64 parameter and return value
        ("F64 Sq(F64 x){ return x*x; } return (I64)Sq(6.0);", 36),
        // mixed int + float parameters (x0 and v0)
        (
            "I64 M(I64 n, F64 f){ return (I64)(n + f); } return M(10, 2.5);",
            12,
        ),
        // eight F64 arguments (all in v0..v7)
        (
            "F64 S(F64 a,F64 b,F64 c,F64 d,F64 e,F64 f,F64 g,F64 h){return a+b+c+d+e+f+g+h;} \
             return (I64)S(1.0,2.0,3.0,4.0,5.0,6.0,7.0,8.0);",
            36,
        ),
        // a global F64 read from a function
        (
            "F64 g = 1.5; F64 D(){ return g * 2.0; } return (I64)D();",
            3,
        ),
        // an F64 local survives a call made while evaluating a binary's operands
        (
            "F64 Id(F64 x){ return x; } F64 a = 10.0; return (I64)(a + Id(5.0));",
            15,
        ),
    ];
    for (src, expected) in cases {
        assert_eq!(build_and_run(src), *expected, "for source: {src}");
    }
}

#[test]
fn float_to_int_conversion_is_implicit() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    // An F64 value flowing into an integer slot is truncated toward zero (no
    // explicit cast needed) — matching the interpreter, not stored as raw bits.
    let cases: &[(&str, i32)] = &[
        ("I64 r = 16.0; return r;", 16),       // declaration init
        ("I64 r; r = 2.7; return r;", 2),      // assignment to an existing int
        ("I64 r = -3.9; return r + 100;", 97), // negative truncates toward zero
        ("I64 Trunc(F64 x){ return x; } return Trunc(7.9);", 7), // arg + return
        ("I64 a[2] = {1.5, 9.5}; return a[0] + a[1];", 10), // aggregate element
        ("return Sqrt(16.0) + Sqrt(9.0);", 7), // builtin result into int return
    ];
    for (src, expected) in cases {
        assert_eq!(build_and_run(src), *expected, "for source: {src}");
    }
}

#[test]
fn compiles_float_printing() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    // %f maps to C's printf %f (six fractional digits).
    let cases: &[(&str, &str)] = &[
        (r#""%f\n", 1.5;"#, "1.500000\n"),
        (r#"F64 x = 2.0; F64 y = 0.5; "%f\n", x + y;"#, "2.500000\n"),
        (r#""%f %f\n", 0.25, 0.75;"#, "0.250000 0.750000\n"),
        // int and float arguments interleaved in one call
        (r#""%d=%f\n", 3, 0.5;"#, "3=0.500000\n"),
    ];
    for (src, expected) in cases {
        assert_eq!(build_and_capture(src), *expected, "for source: {src}");
    }
}

/// The `shapes.hc` sample (F64 fields, methods returning F64, %f printing)
/// compiles natively; output matches a known-good run.
#[test]
fn compiles_shapes_sample() {
    if !toolchain_available() {
        return;
    }
    let out = build_and_capture(include_str!("../examples/shapes.hc"));
    assert_eq!(
        out,
        "rect area = 12.000000\ntri area = 15.000000\ntotal = 39.566371\nrect bigger than tri? 0\n"
    );
}

#[test]
fn compiles_array_parameters() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    let cases: &[(&str, i32)] = &[
        // a 1-D array parameter (decays to a pointer); read its elements
        (
            "I64 Sum(I64 a[], I64 n){ I64 s=0; I64 i; for(i=0;i<n;i++) s+=a[i]; return s; } \
             I64 xs[4]; xs[0]=1; xs[1]=2; xs[2]=3; xs[3]=4; return Sum(xs, 4);",
            10,
        ),
        // writes through an array parameter are visible to the caller (by ref)
        (
            "U0 Fill(I64 a[], I64 n){ I64 i; for(i=0;i<n;i++) a[i]=i*i; } \
             I64 xs[5]; Fill(xs, 5); return xs[4];",
            16,
        ),
        // a 2-D array parameter, indexed twice (row stride = inner size)
        (
            "I64 At(I64 m[3][3], I64 i, I64 j){ return m[i][j]; } \
             I64 g[3][3]; g[1][2]=7; return At(g, 1, 2);",
            7,
        ),
        // mutate a 2-D array parameter, observe it in the caller
        (
            "U0 SetDiag(I64 m[3][3], I64 v){ I64 i; for(i=0;i<3;i++) m[i][i]=v; } \
             I64 g[3][3]; SetDiag(g, 5); return g[0][0] + g[1][1] + g[2][2];",
            15,
        ),
    ];
    for (src, expected) in cases {
        assert_eq!(build_and_run(src), *expected, "for source: {src}");
    }
}

/// The `matrix.hc` sample (2-D `F64` array params passed by reference, nested
/// loops, F64 arithmetic) compiles natively; output matches a known-good run.
#[test]
fn compiles_matrix_sample() {
    if !toolchain_available() {
        return;
    }
    let out = build_and_capture(include_str!("../examples/matrix.hc"));
    assert_eq!(out, "trace = 6.000000\nc[0][0]=2.000000 c[2][1]=2.000000\n");
}

#[test]
fn compiles_structs_by_value() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    let cases: &[(&str, i32)] = &[
        // a struct argument is a copy: mutating it doesn't touch the caller's
        (
            "class P{I64 x; I64 y;} U0 Clobber(P p){ p.x = 99; } \
             P a; a.x = 3; a.y = 4; Clobber(a); return a.x;",
            3,
        ),
        // ...but the callee sees the passed-in values
        (
            "class P{I64 x; I64 y;} I64 Sum(P p){ p.x = 99; return p.x + p.y; } \
             P a; a.x = 3; a.y = 4; return Sum(a);",
            103,
        ),
        // return a struct by value (sret), then read its fields
        (
            "class P{I64 x; I64 y;} P Mk(I64 a){ P p; p.x = a; p.y = a * 2; return p; } \
             P q = Mk(5); return q.x + q.y;",
            15,
        ),
        // whole-struct assignment is a deep copy (no aliasing)
        (
            "class P{I64 x;} P a; a.x = 7; P b; b = a; b.x = 100; return a.x;",
            7,
        ),
        // a struct larger than 16 bytes passed by value
        (
            "class P{I64 a; I64 b; I64 c;} I64 S(P p){ return p.a + p.b + p.c; } \
             P x; x.a = 1; x.b = 2; x.c = 3; return S(x);",
            6,
        ),
        // nested struct (32 bytes) by value
        (
            "class Pt{I64 x; I64 y;} class Box{Pt lo; Pt hi;} \
             I64 Area(Box b){ return (b.hi.x - b.lo.x) * (b.hi.y - b.lo.y); } \
             Box bx; bx.lo.x=0; bx.lo.y=0; bx.hi.x=6; bx.hi.y=7; return Area(bx);",
            42,
        ),
        // a struct-returning call passed straight into a struct parameter
        (
            "class P{I64 x; I64 y;} P Mk(I64 a){ P p; p.x=a; p.y=a; return p; } \
             I64 SumP(P p){ return p.x + p.y; } return SumP(Mk(9));",
            18,
        ),
        // struct copy-init from another struct
        (
            "class P{I64 x; I64 y;} P a; a.x=10; a.y=20; P b = a; b.x = 0; return a.x + b.y;",
            30,
        ),
    ];
    for (src, expected) in cases {
        assert_eq!(build_and_run(src), *expected, "for source: {src}");
    }
}

#[test]
fn compiles_unions() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    let cases: &[(&str, i32)] = &[
        // write a byte field, read the overlapping whole (other bytes zeroed)
        (
            "union U{ I64 w; U8 b[8]; } U u; u.w = 0; u.b[0] = 42; return u.w;",
            42,
        ),
        // write the whole, read individual bytes (little-endian)
        (
            "union U{ I64 w; U8 b[8]; } U u; u.w = 0x0102; return u.b[0] + u.b[1];",
            3,
        ),
        // an F64 field overlapping an I64 field, read back through a cast
        (
            "union U{ I64 i; F64 d; } U u; u.d = 6.0; return (I64)u.d;",
            6,
        ),
        // a union nested in a struct
        (
            "union U{ I64 w; U8 b[8]; } class P{ U u; I64 t; } \
             P p; p.u.w = 0; p.u.b[0] = 9; p.t = 33; return p.u.w + p.t;",
            42,
        ),
    ];
    for (src, expected) in cases {
        assert_eq!(build_and_run(src), *expected, "for source: {src}");
    }
}

#[test]
fn compiles_embedded_unions() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    let cases: &[(&str, i32)] = &[
        // anonymous union: promoted members, write a byte and read the whole
        (
            "class R{ I64 tag; union{ I64 w; U8 b[8]; }; } \
             R r; r.w = 0; r.b[0] = 42; return r.w;",
            42,
        ),
        // anonymous union: write the whole, read promoted bytes
        (
            "class R{ union{ I64 w; U8 b[8]; }; } R r; r.w = 0x0102; return r.b[0] + r.b[1];",
            3,
        ),
        // inline named union member (accessed as r.u.field)
        (
            "class R{ I64 t; union B{ I64 w; U8 b[8]; } u; } \
             R r; r.t = 40; r.u.w = 0; r.u.b[0] = 2; return r.t + r.u.w;",
            42,
        ),
        // a previously-defined union used as a member with the `union` keyword
        (
            "union V{ I64 i; F64 d; } class R{ I64 t; union V v; } \
             R r; r.t = 2; r.v.i = 40; return r.t + r.v.i;",
            42,
        ),
    ];
    for (src, expected) in cases {
        assert_eq!(build_and_run(src), *expected, "for source: {src}");
    }
}

#[test]
fn compiles_member_access_on_call_result() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    let cases: &[(&str, i32)] = &[
        // scalar fields read directly off a struct-returning call
        (
            "class P{I64 x; I64 y;} P Mk(I64 a, I64 b){ P p; p.x=a; p.y=b; return p; } \
             return Mk(3, 4).x + Mk(3, 4).y;",
            7,
        ),
        // nested member path off a call
        (
            "class Pt{I64 x; I64 y;} class Box{Pt lo; Pt hi;} \
             Box Mk(){ Box b; b.lo.x=1; b.lo.y=2; b.hi.x=3; b.hi.y=4; return b; } \
             return Mk().lo.y * 10 + Mk().hi.x;",
            23,
        ),
        // member-on-call yielding a struct, then read its fields
        (
            "class Pt{I64 x; I64 y;} class Box{Pt lo; Pt hi;} \
             Box Mk(){ Box b; b.lo.x=1; b.lo.y=2; b.hi.x=3; b.hi.y=4; return b; } \
             Pt h = Mk().hi; return h.x * 10 + h.y;",
            34,
        ),
    ];
    for (src, expected) in cases {
        assert_eq!(build_and_run(src), *expected, "for source: {src}");
    }
    // An F64 field off a call result (checked through stdout, since exit codes
    // can't carry a float).
    assert_eq!(
        build_and_capture(
            "class V{F64 fx; F64 fy;} V Mk(){ V v; v.fx=1.5; v.fy=2.5; return v; } \
             \"%f\\n\", Mk().fy;"
        ),
        "2.500000\n"
    );
}

#[test]
fn compiles_stdlib_builtins() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    let cases: &[(&str, i32)] = &[
        ("return Abs(-7) + Abs(5);", 12),                // libc llabs
        ("return StrLen(\"hello\") + StrLen(\"\");", 5), // libc strlen
        ("return StrLen(\"abcd\") * 3;", 12),
        ("return Sqrt(16.0) == 4.0;", 1), // libc sqrt, F64 compare -> I64
        ("return Sqrt(2.0) > 1.41 && Sqrt(2.0) < 1.42;", 1),
        // a char buffer that decays to a pointer
        ("U8 b[8]; b[0]='h'; b[1]='i'; b[2]=0; return StrLen(b);", 2),
        // StrCmp normalized to a sign in {-1, 0, 1} (offset to stay >= 0)
        ("return StrCmp(\"abc\", \"abc\") + 5;", 5),
        ("return StrCmp(\"abc\", \"abd\") + 5;", 4),
        ("return StrCmp(\"abd\", \"abc\") + 5;", 6),
        ("return StrCmp(\"ab\", \"abc\") + 5;", 4), // prefix is smaller
        // MAlloc + StrCpy + StrLen + Free round-trip
        (
            "U8 *b = MAlloc(16); StrCpy(b, \"hey\"); I64 n = StrLen(b); Free(b); return n;",
            3,
        ),
        // StrCpy returns its destination pointer
        ("U8 *b = MAlloc(8); return StrLen(StrCpy(b, \"test\"));", 4),
        // a heap-allocated array of structs, indexed and accessed by field
        (
            "class Pt{I64 x; I64 y;} Pt *ps = MAlloc(sizeof(Pt) * 3); \
             ps[0].x = 4; ps[2].y = 38; return ps[0].x + ps[2].y;",
            42,
        ),
        // a heap-allocated linked-list node reached through `->`
        (
            "class Node{I64 v; Node *next;} \
             Node *h = MAlloc(sizeof(Node)); h->v = 7; h->next = MAlloc(sizeof(Node)); \
             h->next->v = 35; return h->v + h->next->v;",
            42,
        ),
    ];
    for (src, expected) in cases {
        assert_eq!(build_and_run(src), *expected, "for source: {src}");
    }
    // Sqrt formatted output goes through printf with six decimals.
    assert_eq!(build_and_capture("\"%f\\n\", Sqrt(2.0);"), "1.414214\n");
    // A MAlloc'd buffer prints through %s after StrCpy.
    assert_eq!(
        build_and_capture("U8 *b = MAlloc(8); StrCpy(b, \"hi there\"); \"%s\\n\", b;"),
        "hi there\n"
    );
}

#[test]
fn compiles_string_memory_and_math_builtins() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    let cases: &[(&str, i32)] = &[
        // StrCat appends (libc strcat)
        (
            "U8 *s = MAlloc(16); StrCpy(s, \"ab\"); StrCat(s, \"cd\"); return StrLen(s);",
            4,
        ),
        // MemSet fills n bytes (libc memset)
        (
            "U8 *d = MAlloc(8); MemSet(d, 7, 4); return d[0] + d[1] + d[2] + d[3];",
            28,
        ),
        // MemCpy copies n bytes (libc memcpy)
        (
            "U8 *d = MAlloc(8); MemCpy(d, \"AB\", 2); return d[0] + d[1];",
            131, // 'A' + 'B'
        ),
        // ToUpper / ToLower
        ("return ToUpper('a') + ToLower('A');", 162), // 'A' + 'a'
        ("return ToUpper('!') + ToLower('5');", 86),  // unchanged: 33 + 53
        // Sin / Cos / Pow (libm), values agree with the interpreter
        ("return Sin(0.0) == 0.0 && Cos(0.0) == 1.0;", 1),
        ("return (I64)Pow(3.0, 3.0);", 27),
        // MemCmp normalized to a sign (offset to stay >= 0)
        ("return MemCmp(\"abc\", \"abd\", 3) + 5;", 4),
        ("return MemCmp(\"abd\", \"abc\", 3) + 5;", 6),
        // Floor / Ceil / Round (libm)
        ("return (I64)Floor(9.9) + (I64)Ceil(0.1);", 10),
        ("return (I64)Round(2.5) + (I64)Round(2.4);", 5), // half away from zero
        ("return (I64)Floor(-1.5) + 10;", 8),
        // Exp / Ln / Tan (libm)
        (
            "return Exp(0.0) == 1.0 && Ln(1.0) == 0.0 && Tan(0.0) == 0.0;",
            1,
        ),
        // StrFind(haystack, needle) returns a pointer into the haystack (or NULL).
        (
            "U8 *s = MAlloc(16); StrCpy(s, \"abcdef\"); return StrFind(s, \"cd\") - s;",
            2,
        ),
        (
            "U8 *s = MAlloc(16); StrCpy(s, \"abc\"); return StrFind(s, \"zz\") == NULL;",
            1,
        ),
        // inverse trig + base-10 log (libm)
        ("return (I64)(ASin(1.0) * 100.0);", 157), // pi/2 * 100
        ("return (I64)(ACos(0.0) * 100.0);", 157),
        ("return (I64)(ATan(1.0) * 100.0);", 78), // pi/4 * 100
        ("return (I64)(ATan2(1.0, 1.0) * 100.0);", 78),
        ("return (I64)Log10(1000.0);", 3),
        // StrNCmp normalized to a sign (offset to stay >= 0); StrNCpy
        ("return StrNCmp(\"abc\", \"abd\", 2) + 5;", 5), // first 2 chars equal
        ("return StrNCmp(\"abc\", \"abd\", 3) + 5;", 4),
        (
            "U8 *d = MAlloc(8); StrNCpy(d, \"xyz\", 2); d[2] = 0; return StrLen(d);",
            2,
        ),
        // Sign (computed inline) and Fabs (libc fabs)
        ("return Sign(-42) + 10;", 9),
        ("return Sign(0) + 10;", 10),
        ("return Sign(99) + 10;", 11),
        ("return (I64)Fabs(-7.5) + (I64)Fabs(7.5);", 14),
        // RandU64 (deterministic splitmix64 over a hidden global); first value's
        // low byte, and that consecutive draws differ
        ("return RandU64() & 0xFF;", 175),
        ("return RandU64() != RandU64();", 1),
    ];
    for (src, expected) in cases {
        assert_eq!(build_and_run(src), *expected, "for source: {src}");
    }
}

#[test]
fn compiles_function_pointers() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    let cases: &[(&str, i32)] = &[
        // call through a (global) function-pointer variable
        (
            "I64 Add(I64 a, I64 b){ return a + b; } \
             I64 (*fp)(I64, I64) = &Add; return fp(40, 2);",
            42,
        ),
        // reassign the pointer between calls
        (
            "I64 A(I64 x){ return x + 1; } I64 B(I64 x){ return x * 2; } \
             I64 (*fp)(I64); fp = &A; I64 r = fp(10); fp = &B; return r + fp(10);",
            31,
        ),
        // a callback parameter
        (
            "I64 Add(I64 a, I64 b){ return a + b; } \
             I64 Apply(I64 (*op)(I64, I64), I64 x, I64 y){ return op(x, y); } \
             return Apply(&Add, 30, 12);",
            42,
        ),
        // call a parenthesised `&Func` directly
        ("I64 Sq(I64 x){ return x * x; } return (&Sq)(7);", 49),
        // a function pointer in a condition
        (
            "I64 Z(I64 x){ return x; } I64 (*fp)(I64) = &Z; return fp ? fp(7) : 0;",
            7,
        ),
        // an F64 function pointer; its result truncates into the int return
        (
            "F64 Half(F64 x){ return x / 2.0; } F64 (*hp)(F64) = &Half; return hp(9.0);",
            4,
        ),
        // a function-pointer struct field (vtable-style), called through the struct
        (
            "I64 Sq(I64 s){ return s * s; } \
             class Shape{ I64 (*area)(I64); I64 size; } \
             Shape sh; sh.area = &Sq; sh.size = 6; return sh.area(sh.size);",
            36,
        ),
        // an array of function pointers (dispatch table), indexed by a variable
        (
            "I64 Add(I64 a, I64 b){ return a + b; } I64 Mul(I64 a, I64 b){ return a * b; } \
             I64 (*ops[2])(I64, I64); ops[0] = &Add; ops[1] = &Mul; \
             I64 i = 1; return ops[i](6, 7);",
            42,
        ),
    ];
    for (src, expected) in cases {
        assert_eq!(build_and_run(src), *expected, "for source: {src}");
    }
}

#[test]
fn compiles_typedef() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    let cases: &[(&str, i32)] = &[
        // a simple scalar alias
        ("typedef I64 MyInt; MyInt x = 40; return x + 2;", 42),
        // a function-pointer alias
        (
            "typedef I64 (*Fn)(I64); I64 Inc(I64 x){ return x + 1; } \
             Fn f = &Inc; return f(41);",
            42,
        ),
        // a function returning a typedef'd function pointer (unblocked by typedef)
        (
            "typedef I64 (*Fn)(I64); I64 Dbl(I64 x){ return x * 2; } \
             Fn Get(){ return &Dbl; } return Get()(21);",
            42,
        ),
        // a class alias used as a by-value parameter type
        (
            "class PS{ I64 a; I64 b; } typedef PS P; I64 Sum(P p){ return p.a + p.b; } \
             P q; q.a = 40; q.b = 2; return Sum(q);",
            42,
        ),
    ];
    for (src, expected) in cases {
        assert_eq!(build_and_run(src), *expected, "for source: {src}");
    }
}

/// Every example in `examples/` must compile with the native backend and run to
/// byte-for-byte the same output as the interpreter (the conformance oracle).
/// This is the catch-all that keeps new examples — and backend changes — honest.
#[test]
fn native_matches_interp_for_every_example() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    for (name, src) in common::EXAMPLES {
        let program = parse(src).unwrap_or_else(|e| panic!("{name}: parse failed: {e}"));
        let errs = check_program(&program);
        assert!(errs.is_empty(), "{name}: semantic errors: {errs:?}");
        let interp_out =
            run_to_string(&program).unwrap_or_else(|e| panic!("{name}: interp error: {e}"));
        let native_out = build_and_capture(src);
        assert_eq!(
            native_out, interp_out,
            "native != interp for example {name}"
        );
    }
}

#[test]
fn native_matches_stdlib_showcase() {
    if !toolchain_available() {
        eprintln!("skipping: arm64 backend needs aarch64-apple-darwin + cc");
        return;
    }
    // The stdlib showcase sample must produce identical output natively and in
    // the interpreter (its `tests/programs.rs` twin checks the same string).
    assert_eq!(
        build_and_capture(include_str!("../examples/stdlib.hc")),
        "Hello, World! len=13\n\
         HELLO, WORLD!\n\
         cmp=-1 memcmp=0\n\
         OK---\n\
         sqrt=12 pow=1024\n\
         floor=3 ceil=4 round=3\n\
         trig=1\n"
    );
    // The heap-growing vector sample also matches natively.
    assert_eq!(
        build_and_capture(include_str!("../examples/vector.hc")),
        "len=10 cap=16\nfirst=1 last=100 sum=385\n"
    );
    // The text-processing sample (StrFind, word count, uppercase).
    assert_eq!(
        build_and_capture(include_str!("../examples/text.hc")),
        "len=19 words=4\n\
         has_quick=1 has_slow=0\n\
         fox_at=16\n\
         THE QUICK BROWN FOX\n"
    );
    // The hash-map sample (heap nodes, StrCmp keys, chaining).
    assert_eq!(
        build_and_capture(include_str!("../examples/hashmap.hc")),
        "one=1 two=22 three=3 missing=-1\n"
    );
    // The RandU64-driven shuffle produces the same permutation natively.
    assert_eq!(
        build_and_capture(include_str!("../examples/shuffle.hc")),
        "2 6 4 5 8 1 3 9 0 7 \nsum=45\n"
    );
    // The recursive-descent JSON parser builds and queries the same heap tree,
    // including an F64 real, escape decoding, and a `Dump` round-trip (which uses
    // the bracketed `switch [tag]` form) — byte-identical to the interpreter.
    assert_eq!(
        build_and_capture(include_str!("../examples/json.hc")),
        "kind=5 count=9\n\
         name=solomon version=2\n\
         pi: kind=6 x100=314\n\
         tags=3 [holyc, jit]\n\
         path=C:\\tmp\\j q=say \"hi\"\n\
         stable=1 meta=0\n\
         nested.depth=3\n\
         json={\"name\":\"solomon\",\"version\":2,\"pi\":3.14,\
         \"tags\":[\"holyc\",\"rust\",\"jit\"],\
         \"path\":\"C:\\\\tmp\\\\j\",\"q\":\"say \\\"hi\\\"\",\
         \"stable\":true,\"meta\":null,\"nested\":{\"depth\":3}}\n"
    );
    // The StrPrint/CatPrint report (sprintf lowering, aligned columns).
    assert_eq!(
        build_and_capture(include_str!("../examples/report.hc")),
        "Item        Qty    Price     Total\n\
         ----        ---    -----     -----\n\
         Widget        4     2.50     10.00\n\
         Gizmo        10     1.25     12.50\n\
         Sprocket      2     9.99     19.98\n\
         Cog          25     0.40     10.00\n\
         TOTAL        41              52.48\n\
         (245 bytes)\n"
    );
    // The format gallery (every conversion, byte-identical including %e/%g).
    assert_eq!(
        build_and_capture(include_str!("../examples/gallery.hc")),
        "label   |    dec |   hex |     oct |      fixed |          sci | gen\n\
         small   |     42 |    2a |      52 |     42.500 |   4.2500e+01 | 42.5\n\
         big     | 123456 | 1e240 |  361100 | 123456.789 |   1.2346e+05 | 123457\n\
         tiny    |      0 |     0 |       0 |      0.000 |   1.2300e-04 | 0.000123\n\
         neg     |     -7 | fffffffffffffff9 | 1777777777777777777771 |     -7.000 |  -7.0000e+00 | -7\n"
    );
}

#[test]
fn unsupported_constructs_are_rejected() {
    // Beyond the current milestones must error at build time rather than
    // silently miscompile.
    let out = std::env::temp_dir().join("solomon-arm64-should-not-exist");
    for src in [
        "U0 F(I64 a,I64 b,I64 c,I64 d,I64 e,I64 f,I64 g,I64 h,I64 i){}", // >8 integer params
        "U0 G(F64 a,F64 b,F64 c,F64 d,F64 e,F64 f,F64 g,F64 h,F64 i){}", // >8 float params
        "U0 P(){ I64 Q(){ return 1; } }",                                // nested function
    ] {
        let program = parse(src).unwrap();
        let err = match Arm64::new(&out).run(&program) {
            Ok(()) => panic!("expected build to fail for `{src}`"),
            Err(e) => e,
        };
        assert!(
            err.message.contains("arm64 backend"),
            "for `{src}` got: {err}"
        );
    }
}
