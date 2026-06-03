//! Shared test fixtures. (A `tests/common/` subdirectory is *not* compiled as
//! its own test binary, so this is included via `mod common;` in the test
//! crates that need it.)

/// Every HolyC example program under `examples/`, embedded so the tests don't
/// depend on the working directory. The single source of truth for the example
/// list — `tests/examples.rs` runs them through the front end + interpreter, and
/// `tests/arm64.rs` compiles each natively and checks it against the interpreter.
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
];

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
/// `math.hc` is gated on `RandU64` usage, since the rest of it (`Pow`/`Floor`/`Gcd`/
/// `PI`/…) collides with examples that roll their own; `strconv.hc` is gated on
/// `StrToF64` (it pulls in the bignum, so only programs that parse floats want it).
#[allow(dead_code)]
pub fn with_stdlib_prelude(src: &str) -> String {
    let mut prelude = String::from("#include <cstr.hc>\n#include <mem.hc>\n#include <ctype.hc>\n");
    if src.contains("RandU64") && !src.contains("#include <math.hc>") {
        prelude.push_str("#include <math.hc>\n");
    }
    if src.contains("StrToF64") && !src.contains("#include <strconv.hc>") {
        prelude.push_str("#include <strconv.hc>\n");
    }
    prelude.push_str(src);
    prelude
}
