//! Output-level tests for the larger sample programs: they don't just run
//! cleanly, they compute the right answers.

use solomon::interp::run_to_string;
use solomon::parser::parse;

/// Parse and run a sample, returning everything it printed.
fn run(name: &str, src: &str) -> String {
    let program = parse(src).unwrap_or_else(|e| panic!("{name}: parse failed: {e}"));
    run_to_string(&program).unwrap_or_else(|e| panic!("{name}: {e}"))
}

#[test]
fn hello_greets() {
    let out = run("hello.hc", include_str!("../examples/hello.hc"));
    assert_eq!(out, "Hello, World!\nx=42 y=255 ratio=3.140000\n");
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
    let out = run("stdlib.hc", include_str!("../examples/stdlib.hc"));
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
fn vector_grows_on_the_heap() {
    // 10 pushes (squares 1..100) into a vector that starts with capacity 2,
    // doubling to 16; the sum of the first ten squares is 385.
    let out = run("vector.hc", include_str!("../examples/vector.hc"));
    assert_eq!(out, "len=10 cap=16\nfirst=1 last=100 sum=385\n");
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
