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
use solomon::parser::parse;
use solomon::sema::check_program;

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
        ("I64 counter; U0 Inc(){ counter++; } Inc(); Inc(); Inc(); return counter;", 3),
        // global with an initialiser
        ("I64 g = 100; return g + 5;", 105),
        // global array
        ("I64 arr[3]; arr[0]=1; arr[1]=2; arr[2]=3; I64 s=0; I64 i; for(i=0;i<3;i++) s+=arr[i]; return s;", 6),
        // global struct
        ("class P{I64 x; I64 y;} P p; p.x=3; p.y=4; return p.x+p.y;", 7),
        // a linked list over a *global* array pool, walked through pointers
        ("class N{I64 v; N *next;} N pool[3]; \
          pool[0].v=1; pool[0].next=&pool[1]; pool[1].v=2; pool[1].next=&pool[2]; \
          pool[2].v=3; pool[2].next=NULL; \
          N *p=&pool[0]; I64 s=0; while(p!=NULL){ s+=p->v; p=p->next; } return s;", 6),
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
    let out = build_and_capture(include_str!("data/linklist.hc"));
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
        ("I64 M(I64 n, F64 f){ return (I64)(n + f); } return M(10, 2.5);", 12),
        // eight F64 arguments (all in v0..v7)
        (
            "F64 S(F64 a,F64 b,F64 c,F64 d,F64 e,F64 f,F64 g,F64 h){return a+b+c+d+e+f+g+h;} \
             return (I64)S(1.0,2.0,3.0,4.0,5.0,6.0,7.0,8.0);",
            36,
        ),
        // a global F64 read from a function
        ("F64 g = 1.5; F64 D(){ return g * 2.0; } return (I64)D();", 3),
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
    let out = build_and_capture(include_str!("data/shapes.hc"));
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
    let out = build_and_capture(include_str!("data/matrix.hc"));
    assert_eq!(
        out,
        "trace = 6.000000\nc[0][0]=2.000000 c[2][1]=2.000000\n"
    );
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
fn unsupported_constructs_are_rejected() {
    // Beyond the current milestones must error at build time rather than
    // silently miscompile.
    let out = std::env::temp_dir().join("solomon-arm64-should-not-exist");
    for src in [
        "U0 F(I64 a,I64 b,I64 c,I64 d,I64 e,I64 f,I64 g,I64 h,I64 i){}", // >8 integer params
        "U0 G(F64 a,F64 b,F64 c,F64 d,F64 e,F64 f,F64 g,F64 h,F64 i){}", // >8 float params
        "U0 P(){ I64 Q(){ return 1; } }",                               // nested function
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
