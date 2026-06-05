//! Output-level tests for the larger sample programs: they don't just run
//! cleanly, they compute the right answers.

use solomon::interp::run_to_string;
use solomon::parser::parse_with;

mod common;

/// Parse and run a sample, returning everything it printed. Examples carry their
/// own `#include <string.hc>`, resolved against the repo `lib/`.
fn run(name: &str, src: &str) -> String {
    let lib = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lib");
    let program = parse_with(src, std::path::Path::new("."), &[lib])
        .unwrap_or_else(|e| panic!("{name}: parse failed: {e}"));
    run_to_string(&program).unwrap_or_else(|e| panic!("{name}: {e}"))
}

#[test]
fn hello_greets() {
    let out = run("hello.hc", include_str!("../examples/hello.hc"));
    assert_eq!(out, "Hello, World!\nx=42 y=255 ratio=3.140000\n");
}

#[test]
fn tuples_multireturn_index_and_destructure() {
    let out = run("tuples.hc", include_str!("../examples/tuples.hc"));
    assert_eq!(
        out,
        "17 / 5 = 3 rem 2\n\
         23 / 4 = 5 rem 3\n\
         30 / 7 = 4 rem 2\n\
         20 / 3 = 6\n\
         p[0]=1 p[1]=2\n\
         x=1 y=2\n\
         sum=10 prod=21 avg=5.0\n"
    );
}

#[test]
fn varargs_vargc_and_vargv() {
    // A `...` function reads `VargC` (count) and `VargV` (raw 8-byte arg slots),
    // including the zero-argument call `Sum()`.
    let out = run("varargs.hc", include_str!("../examples/varargs.hc"));
    assert_eq!(
        out,
        "sum()        = 0\n\
         sum(1,2,3)   = 6\n\
         sum(10..40)  = 100\n\
         max(4,9,2,7) = 9\n"
    );
}

#[test]
fn wordcount_generic_containers() {
    // A Vec/Hmap-heavy program with inferred type args throughout: word frequencies
    // (Hmap<U8*,I64>), a length histogram (Hmap<I64,I64>), sorted keys (Vec<U8*>),
    // entries sorted by a custom comparator (Vec<HmapKV<U8*,I64>>), and an
    // order-independent value sum. Deterministic (all output is sorted or summed).
    let out = run("wordcount.hc", include_str!("../examples/wordcount.hc"));
    assert_eq!(
        out,
        "tokens=25 distinct=12\n\
         by word:\n\
         \x20 again 1\n  and 1\n  barks 1\n  brown 1\n  dog 3\n  fox 3\n  jumps 2\n\
         \x20 lazy 2\n  over 2\n  quick 2\n  runs 1\n  the 6\n\
         top 3:\n  the x6\n  dog x3\n  fox x3\n\
         by length:\n  len 3: 4\n  len 4: 3\n  len 5: 5\n\
         sum=25\n"
    );
}

#[test]
fn args_reads_argc_argv() {
    // `ArgC`/`ArgV` are ambient globals. `run_to_string` supplies one arg (the
    // program name), so `ArgC == 1` and there are no extra args to echo.
    let out = run("args.hc", include_str!("../examples/args.hc"));
    assert_eq!(out, "argc=1\n(no extra args)\n");
}

// ---- container-library edge cases (interpreter-pinned; the source is shared with the
// arm64-Darwin native-parity tests in tests/arm64_darwin.rs) ----

#[test]
fn sort_handles_edge_inputs_and_bsearch() {
    // single/reverse/duplicate inputs over a heap buffer, the insertion- and
    // quicksort paths, and BSearch hit/miss.
    let out = run("sort_edges", common::LIB_SORT_EDGES);
    assert_eq!(
        out,
        "42 \n\
         1 2 3 4 5 6 \n\
         1 1 1 2 3 3 3 \n\
         f2=1 f9=0 f0=0\n\
         sorted50=1\n"
    );
}

#[test]
fn vec_sort_and_bsearch_indices() {
    let out = run("vec_search", common::LIB_VEC_SEARCH);
    assert_eq!(
        out,
        "1 1 2 3 5 5 9 9 \n\
         i1=1 i9=6 i4=-1 i0=-1 i100=-1\n"
    );
}

#[test]
fn hmap_i64_keys_values_entries() {
    // I64 keys, update + delete, rehash (12 inserts over 8 buckets), then HmapValues
    // (order-independent sum) and HmapEntries sorted by key.
    let out = run("hmap_i64", common::LIB_HMAP_I64);
    assert_eq!(
        out,
        "len=10\n\
         sum=1359\n\
         1=1 2=4 3=9 4=16 5=999 6=36 7=49 8=64 9=81 10=100 \n"
    );
}

#[test]
fn exe_runs_at_compile_time() {
    // `#exe { ... }` runs at compile time and splices its output into the source: a
    // compile-time cos table and generated `pow2_*` constants.
    let out = run("exe.hc", include_str!("../examples/exe.hc"));
    assert_eq!(
        out,
        "cos0=1.0000 cos2=0.0000 cos4=-1.0000\n\
         pow2: 1 2 4 8 16\n"
    );
}

#[test]
fn calloc_zeroes() {
    let out = run(
        "calloc",
        "#include <mem.hc>\nU0 Main(){ I64 *a=CAlloc(3*sizeof(I64)); \
         \"%d %d %d\\n\", a[0], a[1], a[2]; Free(a); } Main;",
    );
    assert_eq!(out, "0 0 0\n");
}

#[test]
fn generic_classes_monomorphize() {
    let out = run("generic.hc", include_str!("../examples/generic.hc"));
    assert_eq!(
        out,
        "ints: len=3 max=30\n\
         flts: 1.5 + 2.5 = 4.0\n"
    );
}

#[test]
fn hmap_enumeration_on_empty_map() {
    let out = run("hmap_empty", common::LIB_HMAP_EMPTY);
    assert_eq!(out, "len=0 k=0 v=0 e=0\nget=0 del=0 has=0\n");
}

#[test]
fn exit_halts_execution() {
    // `Exit` (the ambient builtin) stops the program at the call — output before it is
    // flushed, nothing after runs.
    let out = run(
        "exit",
        "#include <os.hc>\nU0 Main() { \"a\\n\"; Exit(3); \"b\\n\"; } Main;",
    );
    assert_eq!(out, "a\n");
}

#[test]
fn sort_orders_ints_and_strings_generically() {
    let out = run("sort.hc", include_str!("../examples/sort.hc"));
    assert_eq!(
        out,
        "0 1 2 3 4 5 6 7 8 9 \n\
         find 7 -> 7\n\
         find 100 -> -1\n\
         apple banana cherry pear \n"
    );
}

#[test]
fn linklist_sorts_and_computes() {
    let out = run("linklist.hc", include_str!("../examples/linklist.hc"));
    assert_eq!(out, "sorted: 1 2 3 5 7 8 9 \nlength=7 gcd(48,36)=12\n");
}

#[test]
fn vm_evaluates_program() {
    // -((2 + 3) * 4 - 5) = -15
    let out = run("vm.hc", include_str!("../examples/vm.hc"));
    assert_eq!(out, "vm result = -15\n");
}

#[test]
fn mathlib_macros_and_algorithms() {
    let out = run("mathlib.hc", include_str!("../examples/mathlib.hc"));
    assert_eq!(
        out,
        "abs=7 max=12 min=-7 clamp=10\n\
         sq6=36 ipow=1024 isqrt=12 popcount=8\n\
         fancy enabled\n\
         release build\n"
    );
}

#[test]
fn shapes_dispatch_and_inheritance() {
    let out = run("shapes.hc", include_str!("../examples/shapes.hc"));
    // rect = 3*4 = 12, tri = 0.5*6*5 = 15, rect not bigger than tri (12 < 15).
    // `%f` prints six decimals (matching libc / the native backend).
    assert!(out.contains("rect area = 12.000000\n"), "got: {out}");
    assert!(out.contains("tri area = 15.000000\n"), "got: {out}");
    assert!(out.contains("rect bigger than tri? 0\n"), "got: {out}");
}

#[test]
fn matrix_multiply_trace() {
    let out = run("matrix.hc", include_str!("../examples/matrix.hc"));
    // (2I)(ones) is all 2s, so trace = 6 and each entry is 2.
    assert!(out.contains("trace = 6.000000\n"), "got: {out}");
    assert!(
        out.contains("c[0][0]=2.000000 c[2][1]=2.000000\n"),
        "got: {out}"
    );
}

#[test]
fn stdlib_builtins_showcase() {
    let out = run("builtin.hc", include_str!("../examples/builtin.hc"));
    assert_eq!(
        out,
        "Hello, World! len=13\n\
         HELLO, WORLD!\n\
         cmp=-1 memcmp=0\n\
         OK---\n\
         sqrt=12\n"
    );
}

#[test]
fn text_word_count_and_search() {
    let out = run("text.hc", include_str!("../examples/text.hc"));
    assert_eq!(
        out,
        "len=19 words=4\n\
         has_quick=1 has_slow=0\n\
         fox_at=16\n\
         THE QUICK BROWN FOX\n"
    );
}

#[test]
fn hashmap_put_get_and_update() {
    // A string->int hash map with chaining; "two" is overwritten (2 -> 22) and a
    // missing key returns the sentinel.
    let out = run("hashmap.hc", include_str!("../examples/hashmap.hc"));
    assert_eq!(out, "one=1 two=22 three=3 missing=-1\n");
}

#[test]
fn shuffle_is_deterministic() {
    // A Fisher-Yates shuffle driven by the seeded RandU64 — the same permutation
    // every run (and in both backends), still summing to 0+...+9 == 45.
    let out = run("shuffle.hc", include_str!("../examples/shuffle.hc"));
    assert_eq!(out, "2 6 4 5 8 1 3 9 0 7 \nsum=45\n");
}

#[test]
fn json_parser_round_trips() {
    // A recursive-descent JSON parser builds a heap tree of tagged JVal nodes and
    // queries it: object kind/arity, string and integer fields, an F64 real
    // (`pi` kind 6, 3.14 -> rounded *100 == 314), array access, escape decoding
    // (`\\` -> `\`, `\"` -> `"`), bool/null tags, and a nested-object lookup —
    // then re-serializes the whole tree back to JSON (`Dump`).
    let out = run("json.hc", include_str!("../examples/json.hc"));
    assert_eq!(
        out,
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
}

#[test]
fn report_builds_aligned_table() {
    // StrPrint/CatPrint compose a formatted sales report with aligned columns
    // (`%-10s`, `%4d`) and fixed-point money (`%8.2f`).
    let out = run("report.hc", include_str!("../examples/report.hc"));
    assert_eq!(
        out,
        "Item        Qty    Price     Total\n\
         ----        ---    -----     -----\n\
         Widget        4     2.50     10.00\n\
         Gizmo        10     1.25     12.50\n\
         Sprocket      2     9.99     19.98\n\
         Cog          25     0.40     10.00\n\
         TOTAL        41              52.48\n\
         (245 bytes)\n"
    );
}

#[test]
fn gallery_renders_every_format() {
    // One value rendered as decimal/hex/octal/fixed/scientific/general.
    let out = run("gallery.hc", include_str!("../examples/gallery.hc"));
    assert_eq!(
        out,
        "label   |    dec |   hex |     oct |      fixed |          sci | gen\n\
         small   |     42 |    2a |      52 |     42.500 |   4.2500e+01 | 42.5\n\
         big     | 123456 | 1e240 |  361100 | 123456.789 |   1.2346e+05 | 123457\n\
         tiny    |      0 |     0 |       0 |      0.000 |   1.2300e-04 | 0.000123\n\
         neg     |     -7 | fffffffffffffff9 | 1777777777777777777771 |     -7.000 |  -7.0000e+00 | -7\n"
    );
}
