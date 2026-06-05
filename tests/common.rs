//! Shared test fixtures. (A `tests/common/` subdirectory is *not* compiled as
//! its own test binary, so this is included via `mod common;` in the test
//! crates that need it.)

/// Every HolyC example program under `examples/`, embedded so the tests don't
/// depend on the working directory. The single source of truth for the example
/// list — `tests/examples.rs` runs them through the front end + interpreter, and
/// `tests/arm64.rs` compiles each natively and checks it against the interpreter.
#[allow(dead_code)]
pub const EXAMPLES: &[(&str, &str)] = &[
    ("hello.hc", include_str!("../examples/hello.hc")),
    ("fib.hc", include_str!("../examples/fib.hc")),
    ("classes.hc", include_str!("../examples/classes.hc")),
    ("control.hc", include_str!("../examples/control.hc")),
    ("preproc.hc", include_str!("../examples/preproc.hc")),
    ("linklist.hc", include_str!("../examples/linklist.hc")),
    ("shapes.hc", include_str!("../examples/shapes.hc")),
    ("vm.hc", include_str!("../examples/vm.hc")),
    ("mathlib.hc", include_str!("../examples/mathlib.hc")),
    ("matrix.hc", include_str!("../examples/matrix.hc")),
    ("builtin.hc", include_str!("../examples/builtin.hc")),
    ("vector.hc", include_str!("../examples/vector.hc")),
    ("text.hc", include_str!("../examples/text.hc")),
    ("hashmap.hc", include_str!("../examples/hashmap.hc")),
    ("shuffle.hc", include_str!("../examples/shuffle.hc")),
    ("json.hc", include_str!("../examples/json.hc")),
    ("report.hc", include_str!("../examples/report.hc")),
    ("gallery.hc", include_str!("../examples/gallery.hc")),
    ("tuples.hc", include_str!("../examples/tuples.hc")),
    ("hmap.hc", include_str!("../examples/hmap.hc")),
    ("sort.hc", include_str!("../examples/sort.hc")),
    ("generic.hc", include_str!("../examples/generic.hc")),
    ("exe.hc", include_str!("../examples/exe.hc")),
];

// ---- container-library edge-case programs ----
//
// Shared by the interpreter-pinned exact-output tests (`tests/programs.rs`, run on
// every host) and the arm64-Darwin native-parity tests (`tests/arm64_darwin.rs`, run
// on an Apple-silicon Mac), so both exercise identical source. They cover the
// `<sort.hc>`/`<vec.hc>`/`<hmap.hc>` surface beyond the happy path the examples show:
// empty/single/reverse/duplicate inputs, search boundaries, the quicksort (>cutoff)
// path, I64 keys, rehash/update/delete, and the `HmapValues`/`HmapEntries` collectors.
// Sorted bases are **heap** buffers (`MAlloc`/`Vec`): the interpreter byte-addresses
// heap blocks but not stack arrays, so a raw `I64 a[N]` would not be a valid base.

#[allow(dead_code)]
pub const LIB_SORT_EDGES: &str = r#"
#include <sort.hc>
#include <mem.hc>
I64 Cmp(U8 *a, U8 *b) { I64 x = *(I64 *)a, y = *(I64 *)b; return x < y ? -1 : x > y; }
U0 PrintBuf(I64 *a, I64 n) { I64 i; for (i = 0; i < n; i++) "%d ", a[i]; "\n"; }
U0 Main()
{
  I64 *one = MAlloc(sizeof(I64)); one[0] = 42;
  Sort(one, 1, sizeof(I64), &Cmp); PrintBuf(one, 1); Free(one);

  I64 i;
  I64 *r = MAlloc(6 * sizeof(I64));
  for (i = 0; i < 6; i++) r[i] = 6 - i;
  Sort(r, 6, sizeof(I64), &Cmp); PrintBuf(r, 6); Free(r);

  I64 *d = MAlloc(7 * sizeof(I64));
  d[0]=3; d[1]=1; d[2]=3; d[3]=1; d[4]=2; d[5]=3; d[6]=1;
  Sort(d, 7, sizeof(I64), &Cmp); PrintBuf(d, 7);
  I64 k;
  k=2; "f2=%d ",  BSearch(&k, d, 7, sizeof(I64), &Cmp) != NULL;
  k=9; "f9=%d ",  BSearch(&k, d, 7, sizeof(I64), &Cmp) != NULL;
  k=0; "f0=%d\n", BSearch(&k, d, 7, sizeof(I64), &Cmp) != NULL;
  Free(d);

  I64 n = 50; I64 *big = MAlloc(n * sizeof(I64));
  for (i = 0; i < n; i++) big[i] = (i * 37 + 11) % 100;
  Sort(big, n, sizeof(I64), &Cmp);
  I64 ok = 1;
  for (i = 1; i < n; i++) if (big[i-1] > big[i]) ok = 0;
  "sorted50=%d\n", ok;
  Free(big);
}
Main;
"#;

#[allow(dead_code)]
pub const LIB_VEC_SEARCH: &str = r#"
#include <vec.hc>
U0 Main()
{
  Vec<I64> v; VecInit(&v);
  VecPush(&v, 5); VecPush(&v, 5); VecPush(&v, 3); VecPush(&v, 9);
  VecPush(&v, 1); VecPush(&v, 1); VecPush(&v, 9); VecPush(&v, 2);
  VecSort(&v, &CmpI64);
  I64 i; for (i = 0; i < VecLen(&v); i++) "%d ", VecAt(&v, i); "\n";
  I64 k;
  k=1;   "i1=%d ",    VecBSearch(&v, &k, &CmpI64);
  k=9;   "i9=%d ",    VecBSearch(&v, &k, &CmpI64);
  k=4;   "i4=%d ",    VecBSearch(&v, &k, &CmpI64);
  k=0;   "i0=%d ",    VecBSearch(&v, &k, &CmpI64);
  k=100; "i100=%d\n", VecBSearch(&v, &k, &CmpI64);
  VecFree(&v);
}
Main;
"#;

#[allow(dead_code)]
pub const LIB_HMAP_I64: &str = r#"
#include <hmap.hc>
U0 Main()
{
  Hmap<I64, I64> m;
  HmapInit(&m, &HmapI64Hash, &HmapI64Eq);
  I64 i;
  for (i = 0; i < 12; i++) HmapPut(&m, i, i * i);
  HmapPut(&m, 5, 999);
  HmapDel(&m, 0);
  HmapDel(&m, 11);
  "len=%d\n", HmapLen(&m);
  Vec<I64> vals; HmapValues(&m, &vals);
  I64 s = 0; for (i = 0; i < VecLen(&vals); i++) s += VecAt(&vals, i);
  "sum=%d\n", s; VecFree(&vals);
  Vec<HmapKV<I64, I64>> e; HmapEntries(&m, &e); VecSort(&e, &CmpI64);
  for (i = 0; i < VecLen(&e); i++) { HmapKV<I64, I64> *p = VecRef(&e, i); "%d=%d ", p->key, p->val; }
  "\n";
  VecFree(&e); HmapFree(&m);
}
Main;
"#;

#[allow(dead_code)]
pub const LIB_HMAP_EMPTY: &str = r#"
#include <hmap.hc>
U0 Main()
{
  Hmap<I64, I64> m;
  HmapInit(&m, &HmapI64Hash, &HmapI64Eq);
  Vec<I64> k, v; Vec<HmapKV<I64, I64>> e;
  HmapKeys(&m, &k); HmapValues(&m, &v); HmapEntries(&m, &e);
  "len=%d k=%d v=%d e=%d\n", HmapLen(&m), VecLen(&k), VecLen(&v), VecLen(&e);
  I64 x = 7; (I64 val, Bool ok) = HmapGet(&m, x);
  "get=%d del=%d has=%d\n", ok, HmapDel(&m, x), HmapHas(&m, x);
  VecFree(&k); VecFree(&v); VecFree(&e); HmapFree(&m);
}
Main;
"#;

/// Parse an example/source with the standard library on the angle-include search
/// path (so `#include <cstr.hc>` etc. resolve to the repo `lib/`). The reducible
/// builtins now live in the HolyC standard library — `lib/cstr.hc` (C strings),
/// `lib/mem.hc` (memory + `ReAlloc`), `lib/ctype.hc` (classification), and the math
/// + `RandU64` PRNG in `lib/math.hc`. Example files carry their own includes, while
/// the many inline test sources do not, so this prepends the primitive modules. The
/// extra unused defs don't affect a program's output.
#[allow(dead_code)]
pub fn parse_example(src: &str) -> Result<solomon::Program, solomon::ParseError> {
    let lib = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lib");
    let src = with_stdlib_prelude(src);
    solomon::parser::parse_with(&src, std::path::Path::new("."), &[lib])
}

/// Prepend the stdlib primitive modules an inline test source may use (`cstr.hc`,
/// `mem.hc`, `ctype.hc`) plus `math.hc` when it uses `RandU64` and `strconv.hc`
/// when it uses `StrToF64`.
///
/// The string/memory/ctype modules are prepended unconditionally — they're guarded
/// (so re-including is a no-op) and define no name any example/test collides with.
/// `math.hc` is gated on `Abs`/`Fabs`/`Sqrt`/`Sign` usage, since the rest of it
/// (`Pow`/`Floor`/`Gcd`/`PI`/…) collides with examples that roll their own; `rand.hc`
/// on `RandU64`; `strconv.hc` on `StrToF64` (it pulls in the bignum); `time.hc` on the
/// clock primitives.
#[allow(dead_code)]
pub fn with_stdlib_prelude(src: &str) -> String {
    let mut prelude = String::from("#include <cstr.hc>\n#include <mem.hc>\n#include <ctype.hc>\n");
    if (src.contains("Abs") || src.contains("Fabs") || src.contains("Sqrt") || src.contains("Sign"))
        && !src.contains("#include <math.hc>")
    {
        prelude.push_str("#include <math.hc>\n");
    }
    if src.contains("RandU64") && !src.contains("#include <rand.hc>") {
        prelude.push_str("#include <rand.hc>\n");
    }
    if src.contains("StrToF64") && !src.contains("#include <strconv.hc>") {
        prelude.push_str("#include <strconv.hc>\n");
    }
    // `time.hc` holds the clock intrinsics (and calendar math), gated on use so its
    // `DateTime`/`FromUnix`/… don't collide with tests/examples that roll their own.
    if (src.contains("UnixNS") || src.contains("NanoNS") || src.contains("Sleep"))
        && !src.contains("#include <time.hc>")
    {
        prelude.push_str("#include <time.hc>\n");
    }
    prelude.push_str(src);
    prelude
}
