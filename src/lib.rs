//! Solomon: a reimplementation of HolyC.
//!
//! A full compiler front end — lexer, preprocessor, parser, semantic analysis,
//! and type layout — feeding a tree-walking [`Interpreter`] (the conformance
//! oracle) and two native code generators behind the [`Codegen`] trait, each
//! named for its target: [`Arm64Darwin`] (`aarch64-apple-darwin`) and
//! [`X64Linux`] (`x86_64-unknown-linux`). The native backends match the
//! interpreter byte-for-byte.

pub mod arm64;
pub mod ast;
pub mod backend;
pub mod builtins;
pub mod codegen;
pub mod fmt;
pub mod interp;
pub mod intrinsics;
pub mod layout;
pub mod lexer;
pub mod mono;
pub mod parser;
pub mod preproc;
pub mod sema;
pub mod token;
pub mod x86_64;

pub use arm64::{Arm64Darwin, Arm64Linux};
pub use ast::{Expr, ExprKind, Program, Stmt, StmtKind, Type};
pub use codegen::{Codegen, CodegenError};
pub use interp::Interpreter;
pub use layout::{Layout, Layouts};
pub use lexer::{LexError, Lexer, TokenStream, tokenize};
pub use mono::MonoError;
pub use parser::{ParseError, Parser, parse};
pub use preproc::Preprocessor;
pub use sema::{SemaError, analyze, check_program};
pub use token::{Keyword, Pos, Span, Token, TokenKind};
pub use x86_64::{X64Linux, X64Windows};

/// The default standard-library search directories for **angle** includes
/// (`#include <math.hc>`), tried in order: the `SOLOMON_STDLIB` environment
/// variable (`:`-separated) if set, then `lib/` resolved relative to the running
/// executable (covering both an installed `<prefix>/bin/hcc` → `<prefix>/lib`
/// layout and the dev `target/<profile>/hcc` → repo `lib/` layout), then `./lib`.
/// Non-existent entries are simply skipped at resolution time.
pub fn stdlib_dirs() -> Vec<std::path::PathBuf> {
    use std::path::PathBuf;
    // The standard library is embedded in the compiler (see `EMBEDDED_STDLIB`), so a
    // `lib/` directory on disk is no longer required. This returns only the override
    // dirs from `SOLOMON_STDLIB` (searched before the embedded copy), for developing
    // against a working tree's `lib/` without recompiling the compiler.
    std::env::var("SOLOMON_STDLIB")
        .ok()
        .into_iter()
        .flat_map(|env| {
            env.split(':')
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The HolyC standard library (`lib/*.hc`), embedded into the compiler **at build
/// time** via `include_str!`, as `(angle-include name, source)` pairs. An angle
/// include `#include <name>` resolves here when the filesystem search path
/// (`SOLOMON_STDLIB`, `-I`) doesn't provide it — so the compiler is self-contained
/// and needs no `lib/` on disk. Editing a `lib/*.hc` file triggers a recompile
/// (Cargo tracks `include_str!` inputs), keeping the embedded copy in sync.
pub const EMBEDDED_STDLIB: &[(&str, &str)] = &[
    ("builtin.hc", include_str!("../lib/builtin.hc")),
    ("cstr.hc", include_str!("../lib/cstr.hc")),
    ("ctype.hc", include_str!("../lib/ctype.hc")),
    ("mem.hc", include_str!("../lib/mem.hc")),
    ("vec.hc", include_str!("../lib/vec.hc")),
    ("sort.hc", include_str!("../lib/sort.hc")),
    ("hmap.hc", include_str!("../lib/hmap.hc")),
    ("_impl/strhash.hc", include_str!("../lib/_impl/strhash.hc")),
    ("fmt.hc", include_str!("../lib/fmt.hc")),
    ("_impl/fltfmt.hc", include_str!("../lib/_impl/fltfmt.hc")),
    ("_impl/printf.hc", include_str!("../lib/_impl/printf.hc")),
    ("bignum.hc", include_str!("../lib/bignum.hc")),
    ("strconv.hc", include_str!("../lib/strconv.hc")),
    ("bits.hc", include_str!("../lib/bits.hc")),
    ("math.hc", include_str!("../lib/math.hc")),
    ("special.hc", include_str!("../lib/special.hc")),
    ("rand.hc", include_str!("../lib/rand.hc")),
    ("time.hc", include_str!("../lib/time.hc")),
    ("io.hc", include_str!("../lib/io.hc")),
    ("os.hc", include_str!("../lib/os.hc")),
    ("net.hc", include_str!("../lib/net.hc")),
    ("thread.hc", include_str!("../lib/thread.hc")),
    ("sync.hc", include_str!("../lib/sync.hc")),
];

/// The embedded source for stdlib angle-include `name`, or `None` if it isn't a
/// bundled standard-library module.
pub fn embedded_stdlib(name: &str) -> Option<&'static str> {
    EMBEDDED_STDLIB
        .iter()
        .find(|(n, _)| *n == name)
        .map(|&(_, src)| src)
}
