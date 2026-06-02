//! Tests for the x86-64 / ELF backend.
//!
//! The structural checks run everywhere (they only inspect the produced ELF
//! image). The end-to-end "run it and check the exit code" test executes the
//! emitted Linux/x86-64 static ELF — directly on a linux/x86_64 host, otherwise
//! in one `docker run --platform linux/amd64` container — and self-skips when
//! neither is available.

use std::sync::atomic::{AtomicU32, Ordering};

use solomon::codegen::Codegen;
use solomon::interp::run_to_string;
use solomon::parser::{parse, parse_with};
use solomon::sema::check_program;
use solomon::x86_64::X64Linux;

/// The ELF header (64) + one program header (56) precede the code.
const CODE_OFFSET: usize = 120;
const VADDR: u64 = 0x40_0000;

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// A temp path unique per call (tests run in parallel).
fn temp_out() -> std::path::PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("solomon-x64-{}-{id}", std::process::id()))
}

/// Parse a program/source with the standard library available (so `#include
/// <string.hc>` resolves). Examples carry the include; inline sources don't, so it
/// is prepended when absent (the moved string builtins now live in `lib/string.hc`;
/// the extra unused defs don't change a program's output).
fn parse_src(src: &str) -> solomon::Program {
    let lib = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lib");
    let owned;
    let s = if src.contains("#include <string.hc>") {
        src
    } else {
        owned = format!("#include <string.hc>\n{src}");
        &owned
    };
    parse_with(s, std::path::Path::new("."), &[lib]).unwrap_or_else(|e| panic!("parse failed: {e}"))
}

/// Compile `src` to the ELF image (written to a temp file, then read back).
fn build_elf(src: &str) -> Vec<u8> {
    let program = parse(src).unwrap_or_else(|e| panic!("parse failed: {e}"));
    let errs = check_program(&program);
    assert!(errs.is_empty(), "semantic errors: {errs:?}");
    let out = temp_out();
    X64Linux::new(&out)
        .run(&program)
        .unwrap_or_else(|e| panic!("x86_64 build failed: {e}"));
    let bytes = std::fs::read(&out).unwrap();
    let _ = std::fs::remove_file(&out);
    bytes
}

fn le_u16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(b[at..at + 2].try_into().unwrap())
}
fn le_u32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
fn le_u64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

#[test]
fn produces_a_valid_elf64_x86_64_executable() {
    let elf = build_elf("return 42;");

    // ELF identification.
    assert_eq!(&elf[0..4], b"\x7FELF", "bad ELF magic");
    assert_eq!(elf[4], 2, "EI_CLASS should be ELFCLASS64");
    assert_eq!(elf[5], 1, "EI_DATA should be little-endian");
    assert_eq!(elf[6], 1, "EI_VERSION");

    // Header fields.
    assert_eq!(le_u16(&elf, 16), 2, "e_type should be ET_EXEC");
    assert_eq!(le_u16(&elf, 18), 0x3E, "e_machine should be EM_X86_64");
    assert_eq!(le_u32(&elf, 20), 1, "e_version");
    assert_eq!(
        le_u64(&elf, 24),
        VADDR + CODE_OFFSET as u64,
        "e_entry should point at the first code byte"
    );
    assert_eq!(le_u64(&elf, 32), 64, "e_phoff (program headers follow)");
    assert_eq!(le_u16(&elf, 52), 64, "e_ehsize");
    assert_eq!(le_u16(&elf, 54), 56, "e_phentsize");
    assert_eq!(le_u16(&elf, 56), 1, "e_phnum (one PT_LOAD)");

    // The single PT_LOAD program header (at offset 64).
    assert_eq!(le_u32(&elf, 64), 1, "p_type should be PT_LOAD");
    assert_eq!(le_u32(&elf, 68), 7, "p_flags should be R|W|X");
    assert_eq!(le_u64(&elf, 72), 0, "p_offset");
    assert_eq!(le_u64(&elf, 80), VADDR, "p_vaddr");
    assert_eq!(le_u64(&elf, 88), VADDR, "p_paddr");
    assert_eq!(
        le_u64(&elf, 96),
        elf.len() as u64,
        "p_filesz should cover the whole file"
    );
    // `return 42;` has no globals, so the BSS is empty and p_memsz == p_filesz.
    assert_eq!(le_u64(&elf, 104), elf.len() as u64, "p_memsz (no BSS here)");
    // p_vaddr ≡ p_offset (mod p_align), so the segment maps cleanly.
    assert_eq!(le_u64(&elf, 112), 0x1000, "p_align");
    assert_eq!(VADDR % 0x1000, le_u64(&elf, 72) % 0x1000);
}

#[test]
fn main_is_framed_and_exits_via_syscall() {
    // `_start` opens a `rbp` frame and the program exits through the `exit`
    // syscall (`mov rax, 60; syscall`). Exact instruction-level behavior is
    // pinned by `programs_run_with_the_expected_exit_code` (which actually runs
    // the binary); this is the host-independent structural guard.
    let code = &build_elf("return 42;")[CODE_OFFSET..];
    #[rustfmt::skip]
    let prologue: &[u8] = &[
        0x55,                   // push rbp
        0x48, 0x89, 0xE5,       // mov rbp, rsp
        0x48, 0x81, 0xEC,       // sub rsp, imm32
    ];
    assert!(
        code.starts_with(prologue),
        "main should start with a frame prologue: {:02X?}",
        &code[..prologue.len().min(code.len())]
    );
    #[rustfmt::skip]
    let exit: &[u8] = &[
        0x48, 0xB8, 60, 0, 0, 0, 0, 0, 0, 0, // mov rax, 60 (SYS_exit)
        0x0F, 0x05,                          // syscall
    ];
    assert!(
        code.ends_with(exit),
        "main should end with the exit syscall: {:02X?}",
        &code[code.len().saturating_sub(exit.len())..]
    );
}

/// Run ELFs `names` in `dir` and return their exit codes — directly on a
/// linux/x86_64 host, otherwise in a single `docker run --platform linux/amd64`
/// container (the static ELF needs no libc, so a bare `alpine` runs it). Returns
/// `None` to skip when neither path is available.
fn run_exit_codes(dir: &std::path::Path, names: &[String]) -> Option<Vec<i32>> {
    use std::process::Command;
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        return names
            .iter()
            .map(|n| {
                Command::new(dir.join(n))
                    .status()
                    .ok()
                    .and_then(|s| s.code())
            })
            .collect();
    }
    let script = names
        .iter()
        .map(|n| format!("/c/{n}; echo $?"))
        .collect::<Vec<_>>()
        .join("\n");
    let out = Command::new("docker")
        .args([
            "run",
            "--platform",
            "linux/amd64",
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
    let codes: Vec<i32> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect();
    (codes.len() == names.len()).then_some(codes)
}

#[test]
fn programs_run_with_the_expected_exit_code() {
    // (source, expected 8-bit exit status). Covers integer expressions, locals
    // and control flow, comparisons / short-circuit logic, and functions
    // (calls, recursion, mutual recursion, six arguments).
    let cases: &[(&str, i32)] = &[
        ("return 0;", 0),
        ("return 2 + 3 * 4 - 5;", 9),
        ("return (1 + 2) * (3 + 4);", 21),
        ("return 100 / 7;", 14),
        ("return 17 % 5;", 2),
        ("return 1 << 5;", 32),
        ("return 65536 >> 9;", 128),
        ("return (12 & 10) | 1;", 9),
        ("return ~0 & 255;", 255),
        ("return -(-42);", 42),
        (
            "I64 s = 0; I64 i; for (i = 1; i <= 10; i++) s += i; return s;",
            55,
        ),
        (
            "I64 n = 5; I64 f = 1; while (n > 1) { f *= n; n--; } return f;",
            120,
        ),
        ("I64 i = 0; do { i++; } while (i < 5); return i;", 5),
        ("I64 x = 3; if (x > 5) return 1; else return 2;", 2),
        ("I64 x = 10; return x > 5 ? 42 : 0;", 42),
        ("I64 a = 0; return (a != 0 && 1 / a) ? 9 : 7;", 7),
        ("I64 a = 1; return (a == 1 || 1 / 0) ? 5 : 6;", 5),
        ("return (3 < 5) + (5 < 3) + (2 == 2);", 2),
        ("I64 x = 0; return !x + !!5;", 2),
        (
            "I64 s=0; I64 i; for(i=0;i<10;i++){ if(i==5) break; if(i%2==0) continue; s+=i; } return s;",
            4,
        ),
        (
            "I64 s=0;I64 i;I64 j;for(i=0;i<3;i++)for(j=0;j<3;j++)s++;return s;",
            9,
        ),
        (
            "I64 Add(I64 a, I64 b){ return a + b; } return Add(40, 2);",
            42,
        ),
        (
            "I64 Fib(I64 n){ if(n<2) return n; return Fib(n-1)+Fib(n-2); } return Fib(10);",
            55,
        ),
        (
            "I64 F(I64 n){ if(n<=1) return 1; return n*F(n-1); } return F(5);",
            120,
        ),
        (
            "I64 S(I64 a,I64 b,I64 c,I64 d,I64 e,I64 f){return a+b+c+d+e+f;} return S(1,2,3,4,5,6);",
            21,
        ),
        (
            "I64 IsEven(I64 n){if(n==0)return 1;return IsOdd(n-1);} \
             I64 IsOdd(I64 n){if(n==0)return 0;return IsEven(n-1);} return IsEven(10);",
            1,
        ),
        // Pointers and arrays: address-of / deref / write-through, indexing in a
        // loop, pointer arithmetic and difference, width-aware narrow elements,
        // 2-D arrays, and array/pointer parameters (by reference).
        ("I64 x = 5; I64 *p = &x; *p = 7; return x;", 7),
        (
            "I64 a[4]; a[0]=10; a[1]=20; a[2]=30; a[3]=40; I64 *p=&a[0]; return *(p+2);",
            30,
        ),
        (
            "I64 a[5]; I64 i; for(i=0;i<5;i++) a[i]=i*i; \
             I64 s=0; for(i=0;i<5;i++) s+=a[i]; return s;",
            30,
        ),
        (
            "I8 b[3]; b[0]=10; b[1]=20; b[2]=30; return b[0]+b[1]+b[2];",
            60,
        ),
        ("U8 x; x = 300; return x;", 44), // narrow store truncates
        ("I64 a[10]; I64 *p=&a[3]; I64 *q=&a[7]; return q-p;", 4),
        (
            "I64 m[2][2]; m[0][0]=1; m[0][1]=2; m[1][0]=3; m[1][1]=4; \
             return m[0][0]+m[0][1]+m[1][0]+m[1][1];",
            10,
        ),
        (
            "I64 Sum(I64 a[], I64 n){ I64 s=0; I64 i; for(i=0;i<n;i++) s+=a[i]; return s; } \
             I64 xs[4]; xs[0]=1; xs[1]=2; xs[2]=3; xs[3]=4; return Sum(xs, 4);",
            10,
        ),
        (
            "U0 Fill(I64 a[], I64 n){ I64 i; for(i=0;i<n;i++) a[i]=i*i; } \
             I64 xs[5]; Fill(xs, 5); return xs[4];",
            16,
        ),
        (
            "U0 SetTo(I64 *p, I64 v){ *p = v; } I64 x; SetTo(&x, 99); return x;",
            99,
        ),
        (
            "I64 a[5]; I64 i; for(i=0;i<5;i++) a[i]=i; \
             I64 *p=a; I64 *q=a+4; I64 s=0; while(p<=q){ s+=*p; p++; } return s;",
            10,
        ),
        // Classes and unions: member access, pointer-to-class (`->`), packed
        // narrow fields + sizeof, nested classes, whole-class assignment and
        // by-value parameters (a deep copy the callee can't observe outside),
        // arrays of classes / heap-free linked lists, and union aliasing
        // (named and anonymous-embedded).
        (
            "class P{I64 x; I64 y;} P p; p.x=3; p.y=4; return p.x+p.y;",
            7,
        ),
        (
            "class P{I64 x; I64 y;} P p; P *pp=&p; pp->x=10; pp->y=20; return pp->x+pp->y;",
            30,
        ),
        (
            "class M{ U8 a; I32 b; U8 c; } M m; m.a=3; m.b=70000; m.c=5; return m.a*10 + m.c;",
            35,
        ),
        ("class M{ U8 a; I32 b; U8 c; } return sizeof(M);", 12),
        (
            "class Pt{I64 x;I64 y;} class Box{Pt lo; Pt hi;} \
             Box b; b.lo.x=1; b.hi.y=9; return b.lo.x+b.hi.y;",
            10,
        ),
        (
            "class P{I64 x;} P a; a.x=7; P b; b=a; b.x=100; return a.x;",
            7,
        ),
        (
            "class P{I64 x;I64 y;} I64 Sum(P p){ p.x=99; return p.x+p.y; } \
             P a; a.x=3; a.y=4; return Sum(a);",
            103,
        ),
        (
            "class P{I64 x;I64 y;} U0 Clobber(P p){ p.x=99; } \
             P a; a.x=3; a.y=4; Clobber(a); return a.x;",
            3,
        ),
        (
            "class N{I64 v; N *next;} N pool[2]; \
             pool[0].v=10; pool[0].next=&pool[1]; pool[1].v=20; pool[1].next=NULL; \
             N *p=&pool[0]; I64 s=0; while(p!=NULL){s+=p->v; p=p->next;} return s;",
            30,
        ),
        (
            "union U{ I64 w; U8 b[8]; } U u; u.w=0; u.b[0]=42; return u.w;",
            42,
        ),
        (
            "union U{ I64 w; U8 b[8]; } U u; u.w=0x0102; return u.b[0]+u.b[1];",
            3,
        ),
        (
            "class R{ I64 tag; union{ I64 w; U8 b[8]; }; } R r; r.w=0; r.b[0]=42; return r.w;",
            42,
        ),
        // Globals: top-level variables live in BSS and are reachable from any
        // function (read, write, `++`, compound-assign, arrays, classes).
        (
            "I64 g; U0 Set(){ g = 42; } I64 Get(){ return g; } Set(); return Get();",
            42,
        ),
        (
            "I64 counter = 5; U0 Bump(){ counter++; } Bump(); Bump(); Bump(); return counter;",
            8,
        ),
        ("I64 a = 10; I64 b = 20; return a + b;", 30),
        (
            "I64 arr[4]; U0 Fill(){ I64 i; for(i=0;i<4;i++) arr[i]=i*10; } \
             Fill(); return arr[0]+arr[1]+arr[2]+arr[3];",
            60,
        ),
        (
            "class P{I64 x; I64 y;} P gp; U0 Init(){ gp.x=3; gp.y=4; } \
             Init(); return gp.x+gp.y;",
            7,
        ),
        ("I64 g; return g;", 0), // an unwritten global reads as 0 (BSS)
        (
            "I64 g = 100; U0 Half(){ g /= 2; } Half(); Half(); return g;",
            25,
        ),
        // F64: arithmetic, comparisons, casts, params/returns, globals, arrays —
        // results truncate to an integer exit code (float printing is separate).
        ("F64 x = 3.5; F64 y = 2.0; return x + y;", 5),
        ("F64 x = 10.0; F64 y = 4.0; return x / y * 20.0;", 50),
        ("F64 x = 2.5; return x * x * 8.0;", 50),
        ("F64 x = -3.9; return -x;", 3), // negate, truncate toward zero
        ("I64 n = 7; F64 x = n; return x * 3.0;", 21), // int → float widening
        ("F64 x = 9.99; return (I64)x;", 9), // float → int (truncate)
        (
            "F64 a = 1.5; F64 b = 2.5; return (a < b) + (b > a) + (a == a);",
            3,
        ),
        (
            "F64 x = 3.14; if (x > 3.0 && x < 4.0) return 42; return 0;",
            42,
        ),
        (
            "F64 Add(F64 a, F64 b){ return a + b; } return Add(1.5, 2.5);",
            4,
        ),
        ("F64 Sq(F64 x){ return x * x; } return Sq(4.0);", 16),
        (
            "F64 Mix(I64 n, F64 f){ return n + f; } return Mix(3, 1.5);",
            4,
        ),
        (
            "F64 g; U0 Set(){ g = 6.5; } I64 Get(){ return g + 0.5; } Set(); return Get();",
            7,
        ),
        ("F64 pi = 3.0; return pi * 2.0;", 6),
        ("F64 x = 1.0; x += 2.5; x *= 2.0; return x;", 7),
        (
            "F64 a[3]; a[0]=1.5; a[1]=2.5; a[2]=3.0; return a[0]+a[1]+a[2];",
            7,
        ),
        (
            "F64 Pow2(I64 n){ if(n==0) return 1.0; return 2.0 * Pow2(n-1); } return Pow2(6);",
            64,
        ),
        ("F64 x = 5e18; U64 u = x; return u % 250;", 0), // unsigned float → int
        // Signedness-directed integer ops (results chosen to fit an 8-bit code).
        ("I64 a = -8; return a >> 1;", 0xFC), // arithmetic shift: -4 & 0xFF
        ("U8 a = 200; return a >> 1;", 100),  // logical shift on unsigned
        ("I64 a = -9; return a / 2;", 0xFC),  // signed div toward zero: -4
        ("I64 a = -7; return a % 3;", 0xFF),  // signed rem: -1
        ("U64 a = 17; return a / 5 + a % 5;", 5), // unsigned div/rem
        ("I64 a = -1; U64 b = 1; return a > b;", 1), // unsigned compare: a huge
        ("I64 a = -1; return a > 1;", 0),     // signed compare
        // Class return by value (sret): a function returns a class through a
        // caller-allocated temp whose address it gets in r11.
        (
            "class P{I64 x; I64 y;} P Mk(I64 a, I64 b){ P p; p.x=a; p.y=b; return p; } \
             P q = Mk(3, 4); return q.x + q.y;",
            7,
        ),
        (
            "class P{I64 x; I64 y;} P Mk(){ P p; p.x=10; p.y=20; return p; } \
             return Mk().x + Mk().y;", // member access on a class-returning call
            30,
        ),
        (
            "class P{I64 x;I64 y;} P Mk(I64 a){ P p; p.x=a; p.y=a*2; return p; } \
             I64 Sum(P p){ return p.x+p.y; } return Sum(Mk(5));", // pass result by value
            15,
        ),
        (
            // Class param followed by another param — exercises the arg-register
            // save order (a class copy must not clobber later args).
            "class P{I64 v;} I64 F(I64 a, P p, I64 b){ return a + p.v + b; } \
             P p; p.v=5; return F(10, p, 100);",
            115,
        ),
        (
            // Accumulate by repeatedly returning a class through a loop.
            "class A{I64 s;I64 n;} A Step(A a, I64 v){ a.s+=v; a.n++; return a; } \
             A s; s.s=0; s.n=0; I64 i; for(i=1;i<=4;i++) s=Step(s,i); return s.s + s.n;",
            14,
        ),
        (
            // sret with a recursive class-returning function.
            "class P{I64 x;} P Build(I64 n){ P p; if(n==0){ p.x=0; return p; } \
             P prev=Build(n-1); p.x=prev.x + n; return p; } return Build(5).x;",
            15,
        ),
        (
            // A class carrying F64 fields, returned by value.
            "class V{F64 x; F64 y;} V Mk(F64 a, F64 b){ V v; v.x=a; v.y=b; return v; } \
             V r = Mk(1.5, 2.5); return (I64)(r.x + r.y);",
            4,
        ),
        // switch: single cases, ranges, default, fall-through, and `break`.
        (
            "I64 Classify(I64 n){ switch(n){ case 0: return 1; case 1 ... 5: return 2; \
             default: return 3; } return 0; } return Classify(0)*100 + Classify(3)*10 + Classify(9);",
            123,
        ),
        (
            "I64 v = 2; I64 r = 0; switch(v){ case 1: r += 1; case 2: r += 2; case 3: r += 4; \
             break; default: r += 100; } return r;", // fall-through 2→3
            6,
        ),
        (
            "I64 s = 0; I64 i; for(i=0;i<6;i++){ switch(i){ case 0 ... 1: s += 1; break; \
             case 2 ... 3: s += 10; break; default: s += 100; } } return s;",
            222,
        ),
        // goto: forward and backward jumps.
        ("I64 x = 0; loop: x++; if (x < 7) goto loop; return x;", 7),
        (
            "I64 s = 0; I64 i; for(i=0;i<10;i++){ if(i==3) goto skip; s += i; skip:; } return s;",
            42, // 0+1+2+4+5+6+7+8+9
        ),
        (
            "I64 x = 5; if (x > 0) goto pos; return 1; pos: return 2;",
            2,
        ),
        // Builtins (no libc — each lowered inline or to an emitted routine).
        (
            "U8 *p = MAlloc(64); I64 i; for(i=0;i<64;i++) p[i]=i; \
             I64 s=0; for(i=0;i<64;i++) s+=p[i]; Free(p); return s & 255;",
            0xE0, // sum 0..63 = 2016; & 255 = 224
        ),
        (
            "U8 b[32]; StrCpy(b, \"hello\"); StrCat(b, \" world\"); return StrLen(b);",
            11,
        ),
        (
            "return (StrCmp(\"abc\",\"abc\")==0) + (StrCmp(\"abc\",\"abd\")<0) \
             + (StrCmp(\"abd\",\"abc\")>0);",
            3,
        ),
        ("U8 *s = \"a.b.c.d\"; return StrLastChr(s, '.') - s;", 5),
        (
            "U8 *s = \"hello world\"; return StrFind(s, \"world\") - s;",
            6,
        ),
        ("return Abs(-42) + Sign(-3) + Sign(0) + Sign(99);", 42),
        ("return ToUpper('a') + ToLower('B') - 'A' - 'b';", 0),
        (
            "U8 a[8]; U8 b[8]; MemSet(a, 7, 8); MemCpy(b, a, 8); \
             return b[0] + b[7] + MemCmp(a, b, 8);",
            14,
        ),
        // RandU64 is a deterministic splitmix64 (seed 0) — same value as the interp.
        ("return RandU64() & 255;", 175), // first splitmix64(0) low byte
        ("return (I64)Sqrt(169.0) + (I64)Fabs(-7.0);", 20),
    ];

    let dir = temp_out();
    std::fs::create_dir_all(&dir).unwrap();
    let names: Vec<String> = cases
        .iter()
        .enumerate()
        .map(|(idx, (src, _))| {
            let program = parse_src(src);
            let errs = check_program(&program);
            assert!(errs.is_empty(), "sema errors for `{src}`: {errs:?}");
            let name = format!("c{idx}");
            X64Linux::new(dir.join(&name))
                .run(&program)
                .unwrap_or_else(|e| panic!("build failed for `{src}`: {e}"));
            name
        })
        .collect();

    let codes = run_exit_codes(&dir, &names);
    let _ = std::fs::remove_dir_all(&dir);
    let Some(codes) = codes else {
        eprintln!("skipping x86-64 execution: needs a linux/x86_64 host or docker (linux/amd64)");
        return;
    };
    for ((src, expected), got) in cases.iter().zip(codes) {
        assert_eq!(got, *expected, "for source: {src}");
    }
}

/// Run each ELF in `dir` and capture its stdout — directly on a linux/x86_64
/// host, otherwise in one docker container (outputs split on a `0x1F` marker
/// printed after each). Returns `None` to skip when neither path is available.
fn run_stdouts(dir: &std::path::Path, names: &[String]) -> Option<Vec<String>> {
    use std::process::Command;
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
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
            "linux/amd64",
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
    let parts: Vec<String> = text.split('\u{1f}').map(|p| p.to_string()).collect();
    (parts.len() > names.len()).then(|| parts[..names.len()].to_vec())
}

#[test]
fn printing_matches_the_interpreter() {
    // The payoff of `Print`/strings: the native backend's stdout must be
    // byte-for-byte the interpreter's (the conformance oracle) — `%d %i %u %x %c
    // %s %%`, a bare string printed verbatim, string-literal args, and printing
    // interleaved with locals/loops/function calls.
    let cases: &[&str] = &[
        r#"U0 Main(){ "Hello, World!\n"; } Main;"#,
        r#"U0 Main(){ "%d %d %d\n", 1, 2, 42; } Main;"#,
        r#"U0 Main(){ "%d %u %x\n", -5, -1, 255; } Main;"#,
        r#"U0 Main(){ "%c%c%c\n", 72, 105, 33; } Main;"#,
        r#"U0 Main(){ "%s, %s! 100%% sure\n", "Hello", "World"; } Main;"#,
        r#"U0 Main(){ "verbatim: 100%%done\n"; } Main;"#, // bare string: %% stays
        r#"I64 Sq(I64 x){ return x*x; } U0 Main(){ I64 i; for(i=1;i<=5;i++) "sq(%d)=%d\n", i, Sq(i); } Main;"#,
        r#"I64 Fib(I64 n){ if(n<2) return n; return Fib(n-1)+Fib(n-2); } U0 Main(){ I64 i; for(i=0;i<12;i++) Print("%d ", Fib(i)); "\n"; } Main;"#,
        r#"U0 Main(){ I64 a[5]; I64 i; for(i=0;i<5;i++) a[i]=i*i; for(i=0;i<5;i++) "%d ", a[i]; "\n"; } Main;"#,
        // Width / precision / flags on integers (mirrors `fmt::render_int`).
        r#"U0 Main(){ "[%5d][%-5d][%05d]\n", 42, 42, 42; } Main;"#,
        r#"U0 Main(){ "[%+d][% d][%+d]\n", 7, 7, -7; } Main;"#,
        r#"U0 Main(){ "[%8.3d][%.5d][%5.2d]\n", 5, 42, 7; } Main;"#,
        r#"U0 Main(){ "[%#x][%#X][%#o][%o]\n", 255, 255, 64, 64; } Main;"#,
        r#"U0 Main(){ "[%08x][%-8x]|[%6X]\n", 255, 255, 4096; } Main;"#,
        r#"U0 Main(){ "%d %x\n", -2147483648, -1; } Main;"#,
        r#"U0 Main(){ "[%.0d][%.0d]\n", 0, 5; } Main;"#,
        // `*` width and precision taken from arguments (incl. a negative width).
        r#"U0 Main(){ "[%*d][%-*d][%.*d][%*.*d]\n", 6, 42, 6, 42, 4, 42, 8, 4, 42; } Main;"#,
        // Width / precision on strings.
        r#"U0 Main(){ "[%10s][%-10s][%.3s]\n", "hi", "hi", "hello"; } Main;"#,
        // Printing through a global variable read inside a function.
        r#"I64 g; U0 Show(){ "g=%d\n", g; } U0 Main(){ g = 99; Show(); g++; Show(); } Main;"#,
        // F64 values reaching `%d` convert to int (the float→int path); float math
        // matches the interpreter bit-for-bit.
        r#"U0 Main(){ F64 x = 3.5; F64 y = 2.0; "%d %d %d\n", (I64)(x+y), (I64)(x*y), (I64)(x/y); } Main;"#,
        r#"F64 Avg(F64 a, F64 b){ return (a+b)/2.0; } U0 Main(){ "%d\n", (I64)(Avg(3.0, 8.0)*10); } Main;"#,
        r#"U0 Main(){ F64 r = 1.0; I64 i; for(i=0;i<10;i++) r *= 1.5; "%d\n", (I64)r; } Main;"#,
        // Signedness-directed `>>` `/` `%` (off the left operand) and relational
        // compares (unsigned if either operand is unsigned) — the high-bit cases
        // diverge between `sar`/`shr`, `idiv`/`div`, and the signed/unsigned ccs.
        r#"U0 Main(){ I64 a = -8; U64 b = 0x8000000000000000; "%d %x\n", a >> 1, b >> 4; } Main;"#,
        r#"U0 Main(){ I64 a = -9; U64 b = 0x8000000000000000; "%d %x\n", a / 2, b / 2; } Main;"#,
        r#"U0 Main(){ I64 a = -7; U64 b = 0x8000000000000001; "%d %d\n", a % 3, b % 2; } Main;"#,
        r#"U0 Main(){ I64 a = -1; U64 b = 1; "%d %d\n", a > b, (-1 > 1); } Main;"#,
        r#"U0 Main(){ U32 a = 4000000000; "%u %u\n", a / 7, a % 7; } Main;"#,
        r#"U0 Main(){ U64 x = 0x8000000000000000; x >>= 4; x /= 2; "%x\n", x; } Main;"#,
        // Builtins through printed output: strings, memory, char/int helpers, RNG.
        r#"U0 Main(){ U8 b[32]; StrCpy(b,"Hello, "); StrCat(b,"World"); "%s (%d)\n", b, StrLen(b); } Main;"#,
        r#"U0 Main(){ U8 b[16]; StrCpy(b,"MixedCase"); StrToUpper(b); "%s ", b; StrToLower(b); "%s\n", b; } Main;"#,
        r#"U0 Main(){ U8 b[16]; StrCpy(b,"abcdef"); StrRev(b); "%s\n", b; } Main;"#,
        r#"U0 Main(){ U8 *p=MAlloc(256); I64 i; for(i=0;i<10;i++) p[i]='A'+i; p[10]=0; "%s\n", p; Free(p); } Main;"#,
        r#"U0 Main(){ I64 i; for(i=0;i<8;i++) "%d ", RandU64() % 100; "\n"; } Main;"#,
        // The sprintf family: format into a buffer (StrPrint), append (CatPrint),
        // and I64ToStr — all via the output sink, then printed back.
        r#"U0 Main(){ U8 b[64]; StrPrint(b, "x=%d [%05d] %s", 3, 42, "ok"); "%s\n", b; } Main;"#,
        r#"U0 Main(){ U8 b[64]; StrCpy(b,"sum:"); I64 i; for(i=1;i<=4;i++) CatPrint(b," +%d", i); "%s\n", b; } Main;"#,
        r#"U0 Main(){ U8 a[32]; U8 b[32]; "%s|%s\n", I64ToStr(123456, a), I64ToStr(-7, b); } Main;"#,
        r#"U0 Main(){ U8 a[32]; U8 b[64]; StrPrint(a,"v%d",7); StrPrint(b,"[%s] then plain", a); "%s\n", b; "stdout still works\n"; } Main;"#,
        // MStrPrint (asprintf into a fresh right-sized buffer) and F64ToStr (`%g`).
        r#"U0 Main(){ U8 *s = MStrPrint("[%d:%s:%.2f]", 7, "hi", 3.14159); "%s\n", s; Free(s); } Main;"#,
        // MStrPrint with output far past the 64-byte initial capacity: forces the
        // growing sink through several reallocations in a single format pass.
        r#"U0 Main(){ U8 *s = MStrPrint("%s/%s/%s", "0123456789ABCDEF0123456789ABCDEF", "0123456789ABCDEF0123456789ABCDEF", "0123456789ABCDEF0123456789ABCDEF"); "%s (%d)\n", s, StrLen(s); Free(s); } Main;"#,
        r#"U0 Main(){ U8 b[64]; F64ToStr(2.71828,b); "%s ",b; F64ToStr(1000000.0,b); "%s ",b; F64ToStr(0.0001,b); "%s\n",b; } Main;"#,
        // The `Is*` ctype predicates — classify each byte of a mixed string; the
        // inline range-check routines must match the interpreter byte-for-byte.
        r#"U0 Main(){ U8 *s = "a1 B!~\t"; I64 i; for(i=0;s[i];i++){ "%d%d%d%d%d ", IsAlpha(s[i]), IsDigit(s[i]), IsSpace(s[i]), IsPunct(s[i]), IsCntrl(s[i]); } "\n"; } Main;"#,
        // %f float printing — correctly rounded (bignum), matching the interpreter
        // (Rust `{:.P}`) byte-for-byte, incl. round-half-to-even ties.
        r#"U0 Main(){ "%f %f %f\n", 3.14159, 0.1, 39.566371; } Main;"#,
        r#"U0 Main(){ "%.2f %.0f %.0f %.0f\n", 2.675, 3.7, 2.5, 3.5; } Main;"#,
        r#"U0 Main(){ "%f %f\n", -3.14, -0.0; } Main;"#,
        r#"U0 Main(){ "%.10f\n", 1.0/3.0; } Main;"#,
        r#"U0 Main(){ "[%10.2f][%-10.2f][%010.2f][%+.2f]\n", 3.14, 3.14, 3.14, 3.14; } Main;"#,
        r#"U0 Main(){ F64 a=10.0,b=3.0; "%f\n", a/b; } Main;"#,
        r#"U0 Main(){ F64 s=0.0; I64 i; for(i=0;i<10;i++) s+=1.1; "%.10f\n", s; } Main;"#,
        // %e / %g scientific & general — significant-digit rounding via the exact
        // decimal expansion, matching the interpreter (Rust `{:.Pe}`) byte-for-byte.
        r#"U0 Main(){ "%e %E %.2e %.0e\n", 1.5, 1234.5, 9.9999996, 9.6; } Main;"#,
        r#"U0 Main(){ "%e %e\n", 1.0e300, 1.0e-300; } Main;"#,
        r#"U0 Main(){ "%g %g %g %g\n", 1.5, 1000000.0, 0.0001, 0.00001; } Main;"#,
        r#"U0 Main(){ "%g %.3g %#g %G\n", 1234567.0, 1234567.0, 1.5, 0.00001; } Main;"#,
        r#"U0 Main(){ "[%12.3e][%-12.3e][%+g][%015.2e]\n", 1.5, 1.5, 2.5, 42.0; } Main;"#,
        // Pathological width/precision: clamped at the shared `fmt` layer (width
        // ≤1024, precision ≤512) so the hand-emitted fixed scratch buffers never
        // overflow. Pre-clamp these segfaulted; they must now match the interpreter.
        r#"U0 Main(){ "%2000d\n", 42; } Main;"#,
        r#"U0 Main(){ "%.800f\n", 3.14; } Main;"#,
        r#"U0 Main(){ "%.100d\n", 7; } Main;"#,
        r#"U0 Main(){ "[%2000s]\n", "tail"; } Main;"#,
        r#"U0 Main(){ "%.700e\n", 1.5; } Main;"#,
    ];

    let dir = temp_out();
    std::fs::create_dir_all(&dir).unwrap();
    let mut names = Vec::new();
    let mut expected = Vec::new();
    for (idx, src) in cases.iter().enumerate() {
        let program = parse_src(src);
        let errs = check_program(&program);
        assert!(errs.is_empty(), "sema errors for `{src}`: {errs:?}");
        let want = run_to_string(&program).unwrap_or_else(|e| panic!("interp error: {e}"));
        let name = format!("p{idx}");
        X64Linux::new(dir.join(&name))
            .run(&program)
            .unwrap_or_else(|e| panic!("build failed for `{src}`: {e}"));
        names.push(name);
        expected.push(want);
    }

    let got = run_stdouts(&dir, &names);
    let _ = std::fs::remove_dir_all(&dir);
    let Some(got) = got else {
        eprintln!("skipping x86-64 print conformance: needs a linux/x86_64 host or docker");
        return;
    };
    for ((src, want), out) in cases.iter().zip(&expected).zip(&got) {
        assert_eq!(out, want, "stdout mismatch for `{src}`");
    }
}

#[test]
fn buildable_examples_match_the_interpreter() {
    // Whole example programs that fall within the implemented subset (integers,
    // control flow, functions, pointers/arrays, printing) compile natively and
    // print exactly what the interpreter does — the same conformance the arm64
    // backend's `native_matches_interp_for_every_example` enforces.
    let examples: &[(&str, &str)] = &[
        ("fib", include_str!("../examples/fib.hc")),
        ("mathlib", include_str!("../examples/mathlib.hc")),
        ("classes", include_str!("../examples/classes.hc")),
        ("linklist", include_str!("../examples/linklist.hc")),
        ("preproc", include_str!("../examples/preproc.hc")),
        ("control", include_str!("../examples/control.hc")),
        ("vm", include_str!("../examples/vm.hc")),
        ("hashmap", include_str!("../examples/hashmap.hc")),
        ("shuffle", include_str!("../examples/shuffle.hc")),
        ("json", include_str!("../examples/json.hc")),
        ("text", include_str!("../examples/text.hc")),
        ("vector", include_str!("../examples/vector.hc")),
        ("shapes", include_str!("../examples/shapes.hc")),
        ("matrix", include_str!("../examples/matrix.hc")),
        ("hello", include_str!("../examples/hello.hc")),
        ("report", include_str!("../examples/report.hc")),
        ("gallery", include_str!("../examples/gallery.hc")),
        ("builtin", include_str!("../examples/builtin.hc")),
    ];
    let dir = temp_out();
    std::fs::create_dir_all(&dir).unwrap();
    let mut names = Vec::new();
    let mut expected = Vec::new();
    for (name, src) in examples {
        let program = parse_src(src);
        assert!(check_program(&program).is_empty(), "{name}: sema errors");
        let want = run_to_string(&program).unwrap_or_else(|e| panic!("{name}: interp error: {e}"));
        X64Linux::new(dir.join(name))
            .run(&program)
            .unwrap_or_else(|e| panic!("{name}: native build failed: {e}"));
        names.push(name.to_string());
        expected.push(want);
    }
    let got = run_stdouts(&dir, &names);
    let _ = std::fs::remove_dir_all(&dir);
    let Some(got) = got else {
        eprintln!("skipping x86-64 example conformance: needs a linux/x86_64 host or docker");
        return;
    };
    for ((name, _), (out, want)) in examples.iter().zip(got.iter().zip(&expected)) {
        assert_eq!(out, want, "native != interp stdout for example {name}");
    }
}

#[test]
fn stdlib_math_matches_the_interpreter() {
    // The HolyC standard library (`#include <math.hc>`) compiles through the
    // native pipeline and prints exactly what the interpreter does — exercising
    // angle includes end-to-end and the F64 algebraic builtins this backend lowers
    // (`Floor`/`Ceil`/`Round` via `roundsd`, `Round` matching the interpreter's
    // round-half-away tie-break byte-for-byte).
    let src = r#"
        #include <math.hc>
        U0 Main() {
          "%.6f %.6f %.6f\n", Exp(1.0), Ln(E), Pow(2.0, 10.0);
          "%.6f %.6f %.6f\n", Sin(PI / 2.0), Cos(0.0), Tan(PI / 4.0);
          "%.6f %.6f %.6f\n", Atan(1.0), Log10(1000.0), Hypot(3.0, 4.0);
          "%.6f %.6f %.6f\n", Sinh(1.0), Asin(0.5), Atan2(1.0, -1.0);
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

    let dir = temp_out();
    std::fs::create_dir_all(&dir).unwrap();
    let name = "stdmath".to_string();
    X64Linux::new(dir.join(&name))
        .run(&program)
        .unwrap_or_else(|e| panic!("native build failed: {e}"));
    let got = run_stdouts(&dir, &[name]);
    let _ = std::fs::remove_dir_all(&dir);
    let Some(got) = got else {
        eprintln!("skipping x86-64 stdlib conformance: needs a linux/x86_64 host or docker");
        return;
    };
    assert_eq!(got[0], want, "native != interp stdout for the math stdlib");
}

#[test]
fn time_builtins_run_natively() {
    // Time is impure (non-reproducible), so it can't be byte-compared to the
    // interpreter — run the native binary and assert *properties*: the wall clock
    // is past 1970 and the monotonic clock doesn't go backwards across a Sleep.
    let src = r#"U0 Main() {
        I64 a = NanoNS();
        Sleep(2000000);
        I64 b = NanoNS();
        "%d %d\n", UnixNS() > 1000000000000000000, b >= a;
    } Main;"#;
    let program = parse_src(src);
    assert!(check_program(&program).is_empty(), "sema errors");
    let dir = temp_out();
    std::fs::create_dir_all(&dir).unwrap();
    let name = "timeprog".to_string();
    X64Linux::new(dir.join(&name))
        .run(&program)
        .unwrap_or_else(|e| panic!("native build failed: {e}"));
    let got = run_stdouts(&dir, &[name]);
    let _ = std::fs::remove_dir_all(&dir);
    let Some(got) = got else {
        eprintln!("skipping x86-64 time conformance: needs a linux/x86_64 host or docker");
        return;
    };
    assert_eq!(got[0], "1 1\n", "time builtin properties hold natively");
}

#[test]
fn variadic_functions_match_the_interpreter() {
    // Varargs are deterministic, so the native vararg ABI (a caller-frame buffer +
    // two hidden args) is held byte-for-byte to the interpreter.
    let src = r#"
        I64 SumI(...) { I64 s=0,i=0,n=VarArgCnt(); while(i<n){s+=VarArgI64(i);i++;} return s; }
        F64 AvgF(...) { F64 s=0.0; I64 i=0,n=VarArgCnt(); while(i<n){s+=VarArgF64(i);i++;} return s/n; }
        U0 Join(U8 *sep, ...) { I64 i=0,n=VarArgCnt(); while(i<n){ if(i)"%s",sep; "%s",VarArg(i); i++; } "\n"; }
        U0 Main() {
          "%d %d\n", SumI(10,20,30,40), SumI(7);
          "%.3f\n", AvgF(1.0,2.0,6.0);
          Join(" | ", "x", "y", "z");
        }
        Main;
    "#;
    let program = parse_src(src);
    assert!(check_program(&program).is_empty(), "sema errors");
    let want = run_to_string(&program).unwrap_or_else(|e| panic!("interp error: {e}"));
    let dir = temp_out();
    std::fs::create_dir_all(&dir).unwrap();
    let name = "varargs".to_string();
    X64Linux::new(dir.join(&name))
        .run(&program)
        .unwrap_or_else(|e| panic!("native build failed: {e}"));
    let got = run_stdouts(&dir, &[name]);
    let _ = std::fs::remove_dir_all(&dir);
    let Some(got) = got else {
        eprintln!("skipping x86-64 varargs conformance: needs a linux/x86_64 host or docker");
        return;
    };
    assert_eq!(got[0], want, "native != interp for varargs");
}

#[test]
fn time_calendar_math_matches_the_interpreter() {
    // The pure calendar math in lib/time.hc (class-by-value return + StrPrint with
    // class fields) is held byte-for-byte to the interpreter.
    let src = r#"
        #include <time.hc>
        U0 Show(I64 s) {
          U8 b[32]; DateTime dt = FromUnix(s);
          "%s w%d r%d\n", FmtISO(b, dt), dt.wday, ToUnix(dt) == s;
        }
        U0 Main() { Show(0); Show(1717200000); Show(1000000000); Show(-86400); }
        Main;
    "#;
    let program = parse_src(src);
    assert!(check_program(&program).is_empty(), "sema errors");
    let want = run_to_string(&program).unwrap_or_else(|e| panic!("interp error: {e}"));
    let dir = temp_out();
    std::fs::create_dir_all(&dir).unwrap();
    let name = "timecal".to_string();
    X64Linux::new(dir.join(&name))
        .run(&program)
        .unwrap_or_else(|e| panic!("native build failed: {e}"));
    let got = run_stdouts(&dir, &[name]);
    let _ = std::fs::remove_dir_all(&dir);
    let Some(got) = got else {
        eprintln!("skipping x86-64 time.hc conformance: needs a linux/x86_64 host or docker");
        return;
    };
    assert_eq!(got[0], want, "native != interp for lib/time.hc");
}
