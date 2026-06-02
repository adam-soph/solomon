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
/// path (so `#include <string.hc>` resolves to the repo `lib/`). The reducible
/// builtins now live in the HolyC standard library — string/memory/ctype ops in
/// `lib/string.hc`, the math + `RandU64` PRNG in `lib/math.hc`. Example files carry
/// their own includes, while the many inline test sources do not, so this prepends
/// both (each only when absent — never double-including, which would be a
/// redefinition error). The extra unused defs don't affect a program's output.
#[allow(dead_code)]
pub fn parse_example(src: &str) -> Result<solomon::Program, solomon::ParseError> {
    let lib = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lib");
    let src = with_stdlib_prelude(src);
    solomon::parser::parse_with(&src, std::path::Path::new("."), &[lib])
}

/// Prepend the stdlib includes an inline test source needs but doesn't already
/// carry, so it can use the moved string/memory/ctype/`RandU64` library functions.
///
/// `string.hc` is prepended unconditionally (no example/test defines its names),
/// but `math.hc` only when the source uses the moved `RandU64` PRNG — the rest of
/// `math.hc` (`Pow`/`Floor`/`Gcd`/`PI`/…) collides with examples that roll their
/// own, so it must not be prepended wholesale.
#[allow(dead_code)]
pub fn with_stdlib_prelude(src: &str) -> String {
    let mut prelude = String::new();
    if !src.contains("#include <string.hc>") {
        prelude.push_str("#include <string.hc>\n");
    }
    if src.contains("RandU64") && !src.contains("#include <math.hc>") {
        prelude.push_str("#include <math.hc>\n");
    }
    prelude.push_str(src);
    prelude
}
