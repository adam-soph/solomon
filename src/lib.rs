//! Solomon: a reimplementation of HolyC.
//!
//! The crate is a full compiler front end — lexer, preprocessor, parser, semantic
//! analysis, and type layout. It feeds a tree-walking [`Interpreter`], the
//! conformance oracle, and two native code generators behind the [`Codegen`]
//! trait. Each is named for its target: [`Arm64Darwin`] (`aarch64-apple-darwin`)
//! and [`X64Linux`] (`x86_64-unknown-linux`). The native backends match the
//! interpreter byte-for-byte.

pub mod arm64;
pub mod ast;
pub mod codegen;
pub mod interp;
pub mod intrinsics;
pub mod ir;
pub mod irinterp;
pub mod layout;
pub mod lexer;
pub mod lower;
pub mod mono;
pub mod parser;
pub mod preproc;
pub mod regalloc;
pub mod sema;
pub mod token;
pub mod x86_64;

pub use arm64::{Arm64Darwin, Arm64Linux};
pub use ast::{Expr, ExprKind, Program, Stmt, StmtKind, Type};
pub use codegen::{Codegen, CodegenError};
pub use layout::{Layout, Layouts};
pub use lexer::{LexError, Lexer, TokenStream, tokenize};
pub use mono::MonoError;
pub use parser::{ParseError, Parser, parse};
pub use preproc::Preprocessor;
pub use sema::{SemaError, analyze, check_program};
pub use token::{Keyword, Pos, Span, Token, TokenKind};
pub use x86_64::{X64Linux, X64Windows};

/// The default search directories for angle includes (`#include <math.hc>`),
/// tried in order.
///
/// First the `SOLOMON_STDLIB` environment variable, `:`-separated, if set. Then
/// `lib/` resolved relative to the running executable; this covers both an
/// installed `<prefix>/bin/hcc` → `<prefix>/lib` layout and the dev
/// `target/<profile>/hcc` → repo `lib/` layout. Finally `./lib`. Non-existent
/// entries are skipped at resolution time.
pub fn stdlib_dirs() -> Vec<std::path::PathBuf> {
    use std::path::PathBuf;
    // The standard library is embedded in the compiler (see `EMBEDDED_STDLIB`), so
    // a `lib/` directory on disk is no longer required. This returns only the
    // `SOLOMON_STDLIB` override dirs, which are searched before the embedded copy.
    // They let you develop against a working tree's `lib/` without recompiling the
    // compiler.
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

/// The HolyC standard library (`lib/*.hc`), embedded into the compiler at build
/// time via `include_str!`, as `(angle-include name, source)` pairs.
///
/// An angle include `#include <name>` resolves here when the filesystem search
/// path (`SOLOMON_STDLIB`, `-I`) does not provide it, so the compiler is
/// self-contained and needs no `lib/` on disk. Editing a `lib/*.hc` file triggers
/// a recompile, since Cargo tracks `include_str!` inputs, keeping the embedded
/// copy in sync.
pub const EMBEDDED_STDLIB: &[(&str, &str)] = &[
    ("builtin.hc", include_str!("../lib/builtin.hc")),
    // Public C-named headers.
    ("string.hc", include_str!("../lib/string.hc")),
    ("ctype.hc", include_str!("../lib/ctype.hc")),
    ("stdio.hc", include_str!("../lib/stdio.hc")),
    ("stdlib.hc", include_str!("../lib/stdlib.hc")),
    ("math.hc", include_str!("../lib/math.hc")),
    ("time.hc", include_str!("../lib/time.hc")),
    ("fcntl.hc", include_str!("../lib/fcntl.hc")),
    ("unistd.hc", include_str!("../lib/unistd.hc")),
    ("errno.hc", include_str!("../lib/errno.hc")),
    ("limits.hc", include_str!("../lib/limits.hc")),
    ("float.hc", include_str!("../lib/float.hc")),
    ("socket.hc", include_str!("../lib/socket.hc")),
    ("threads.hc", include_str!("../lib/threads.hc")),
    ("stdatomic.hc", include_str!("../lib/stdatomic.hc")),
    // Container extensions (no C equivalent).
    ("vec.hc", include_str!("../lib/vec.hc")),
    ("hmap.hc", include_str!("../lib/hmap.hc")),
];

/// The embedded source for stdlib angle-include `name`, or `None` if it is not a
/// bundled standard-library module.
pub fn embedded_stdlib(name: &str) -> Option<&'static str> {
    EMBEDDED_STDLIB
        .iter()
        .find(|(n, _)| *n == name)
        .map(|&(_, src)| src)
}
