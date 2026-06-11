//! Solomon: a reimplementation of HolyC.
//!
//! The crate is a full compiler front end — lexer, preprocessor, parser, semantic
//! analysis, and type layout — lowering to one SSA [IR](crate::ir). It feeds an IR
//! interpreter ([`crate::irinterp`], the conformance oracle) and four native code
//! generators behind the [`Codegen`] trait, one per (arch, OS) target: [`Arm64Darwin`]
//! (`aarch64-apple-darwin`), [`Arm64Linux`] (`aarch64-unknown-linux`), [`X64Linux`]
//! (`x86_64-unknown-linux`), and [`X64Windows`] (`x86_64-pc-windows`). Every native
//! backend matches the IR interpreter byte-for-byte.

pub mod ast;
pub mod backend;
pub mod frontend;
pub mod intrinsics;
pub mod ir;
pub mod irinterp;
pub mod token;

// The front-end passes live under `frontend` (see that module's pipeline doc).
// Re-export them at the crate root so `crate::parser`, `crate::sema`, … — and the
// external `hcc::parser`, … — keep resolving as before.
pub use frontend::{layout, lexer, lower, mono, parser, preproc, sema};

pub use ast::{Expr, ExprKind, Program, Stmt, StmtKind, Type};
pub use backend::arm64::{Arm64Darwin, Arm64Linux};
pub use backend::x86_64::{X64Linux, X64Windows};
pub use backend::{Codegen, CodegenError};
pub use frontend::layout::{Layout, Layouts};
pub use frontend::lexer::{LexError, Lexer, TokenStream, tokenize};
pub use frontend::mono::MonoError;
pub use frontend::parser::{ParseError, Parser, parse, parse_with_target};
pub use frontend::preproc::Preprocessor;
pub use frontend::sema::{SemaError, analyze, check_program};
pub use token::{Keyword, Pos, Span, Token, TokenKind};

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
    // Platform-specific header (C analog: `<windows.h>`), gated by the predefined
    // `_WIN32` target macro (see `target_macros`).
    ("windows.hc", include_str!("../lib/windows.hc")),
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

/// The compiler-predefined preprocessor macros for a target `triple`, faithful to
/// C's platform macros (`_WIN32`, `__linux__`, `__APPLE__`, `__x86_64__`, …). Each
/// is an object-like macro defined to `1`, seeded into the preprocessor so a
/// program can `#ifdef _WIN32` / `#ifdef __linux__` and select per target — the
/// mechanism a platform header like `<windows.hc>` is gated on.
///
/// `__solomon__` (the compiler marker, C analog `__GNUC__`) is always defined. An
/// unrecognized triple yields just that marker. This is the single source of truth
/// for the policy; the CLI's `Target` and the integration tests both consult it.
pub fn target_macros(triple: &str) -> Vec<(&'static str, &'static str)> {
    let mut m = vec![("__solomon__", "1")];
    let (arch, os) = match triple {
        "aarch64-apple-darwin" => ("aarch64", "darwin"),
        "x86_64-unknown-linux" => ("x86_64", "linux"),
        "aarch64-unknown-linux" => ("aarch64", "linux"),
        "x86_64-pc-windows" => ("x86_64", "windows"),
        _ => return m,
    };
    match arch {
        "x86_64" => m.push(("__x86_64__", "1")),
        "aarch64" => m.push(("__aarch64__", "1")),
        _ => {}
    }
    match os {
        "darwin" => {
            // `__APPLE__` + `__MACH__` is the canonical macOS pair; macOS is a Unix.
            m.push(("__APPLE__", "1"));
            m.push(("__MACH__", "1"));
            m.push(("__unix__", "1"));
        }
        "linux" => {
            m.push(("__linux__", "1"));
            m.push(("__unix__", "1"));
        }
        "windows" => {
            // `_WIN32` is defined on both 32- and 64-bit Windows; `_WIN64` adds 64-bit.
            m.push(("_WIN32", "1"));
            m.push(("_WIN64", "1"));
        }
        _ => {}
    }
    m
}

/// The host's target triple, used by the host-defaulting parse entry points and the
/// interpreter (which executes on the host). `cfg!`-based and decoupled from
/// backend support, so the interpreter on any host still sees that host's OS/arch
/// macros; an unrecognized host yields `""` (the `__solomon__`-only macro set).
pub fn host_triple() -> &'static str {
    if cfg!(all(target_arch = "aarch64", target_os = "macos")) {
        "aarch64-apple-darwin"
    } else if cfg!(all(target_arch = "x86_64", target_os = "linux")) {
        "x86_64-unknown-linux"
    } else if cfg!(all(target_arch = "aarch64", target_os = "linux")) {
        "aarch64-unknown-linux"
    } else if cfg!(all(target_arch = "x86_64", target_os = "windows")) {
        "x86_64-pc-windows"
    } else {
        ""
    }
}
