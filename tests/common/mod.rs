//! Shared test fixtures. (A `tests/common/` subdirectory is *not* compiled as
//! its own test binary, so this is included via `mod common;` in the test
//! crates that need it.)

/// Every HolyC example program under `examples/`, embedded so the tests don't
/// depend on the working directory. The single source of truth for the example
/// list — `tests/examples.rs` runs them through the front end + interpreter, and
/// `tests/arm64.rs` compiles each natively and checks it against the interpreter.
pub const EXAMPLES: &[(&str, &str)] = &[
    ("hello.hc", include_str!("../../examples/hello.hc")),
    ("fib.hc", include_str!("../../examples/fib.hc")),
    ("classes.hc", include_str!("../../examples/classes.hc")),
    ("control.hc", include_str!("../../examples/control.hc")),
    ("preproc.hc", include_str!("../../examples/preproc.hc")),
    ("linklist.hc", include_str!("../../examples/linklist.hc")),
    ("shapes.hc", include_str!("../../examples/shapes.hc")),
    ("vm.hc", include_str!("../../examples/vm.hc")),
    ("mathlib.hc", include_str!("../../examples/mathlib.hc")),
    ("matrix.hc", include_str!("../../examples/matrix.hc")),
    ("stdlib.hc", include_str!("../../examples/stdlib.hc")),
    ("vector.hc", include_str!("../../examples/vector.hc")),
    ("text.hc", include_str!("../../examples/text.hc")),
    ("hashmap.hc", include_str!("../../examples/hashmap.hc")),
    ("shuffle.hc", include_str!("../../examples/shuffle.hc")),
    ("json.hc", include_str!("../../examples/json.hc")),
    ("report.hc", include_str!("../../examples/report.hc")),
    ("gallery.hc", include_str!("../../examples/gallery.hc")),
];
