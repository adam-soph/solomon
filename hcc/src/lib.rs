//! hcc: a reimplementation of HolyC.
//!
//! The crate is a full compiler front end — lexer, preprocessor, parser, semantic
//! analysis, and type layout — lowering to one SSA [IR](crate::ir). It feeds an IR
//! interpreter ([`crate::oracle`], the conformance oracle) and four native code
//! generators behind the [`Codegen`] trait, one per (arch, OS) target: [`Arm64Darwin`]
//! (`aarch64-apple-darwin`), [`Arm64Linux`] (`aarch64-unknown-linux`), [`X64Linux`]
//! (`x86_64-unknown-linux`), and [`X64Windows`] (`x86_64-pc-windows`). Every native
//! backend matches the IR interpreter byte-for-byte.

pub mod ast;
pub mod backend;
pub mod frontend;
pub mod intrinsics;
pub mod ir;
pub mod oracle;
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

/// The search directories for angle includes (`#include <stdio.hh>`) — the HolyC
/// standard library — tried in order. Non-existent entries are skipped when an
/// include is resolved, so listing a path that may not exist is harmless.
///
/// The standard library is **not** embedded in the compiler; it is read from disk.
/// Resolution mirrors Go's `GOROOT` model:
///
///   1. `HCC_STDLIB` — an explicit `:`-separated override, highest priority. Point
///      it at a working tree's `stdlib/` to develop against it.
///   2. `$HCC_ROOT/lib` — the installed standard library. The install scripts set
///      `HCC_ROOT` in your shell profile and copy the library to `$HCC_ROOT/lib`.
///   3. `<exe dir>/../lib` — a relocatable-install fallback, so an install tree with
///      `bin/hcc` beside `lib/` works even with `HCC_ROOT` unset.
///   4. The repo's `stdlib/`, located relative to this crate at build time — a dev
///      fallback that lets `cargo run`/`cargo test` from the source tree find the
///      library with nothing configured. It only exists on the build machine, so it
///      is silently skipped on an installed binary.
pub fn stdlib_dirs() -> Vec<std::path::PathBuf> {
    use std::path::PathBuf;
    let mut dirs: Vec<PathBuf> = Vec::new();
    // 1. HCC_STDLIB: explicit `:`-separated override.
    if let Ok(env) = std::env::var("HCC_STDLIB") {
        dirs.extend(env.split(':').filter(|s| !s.is_empty()).map(PathBuf::from));
    }
    // 2. $HCC_ROOT/lib: the installed standard library (Go's GOROOT model).
    if let Some(root) = std::env::var_os("HCC_ROOT") {
        if !root.is_empty() {
            dirs.push(PathBuf::from(root).join("lib"));
        }
    }
    // 3. <exe dir>/../lib: relocatable-install fallback (`bin/hcc` → `../lib`).
    if let Ok(exe) = std::env::current_exe() {
        if let Some(prefix) = exe.parent().and_then(|p| p.parent()) {
            dirs.push(prefix.join("lib"));
        }
    }
    // 4. Dev fallback: the repo `stdlib/`, relative to this crate at build time. The
    //    crate may sit at the repo root or in a `hcc/` workspace member, so try both
    //    `<crate>/stdlib` and `<crate>/../stdlib`.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    dirs.push(manifest.join("stdlib"));
    if let Some(parent) = manifest.parent() {
        dirs.push(parent.join("stdlib"));
    }
    dirs
}

/// The compiler-predefined preprocessor macros for a target `triple`, faithful to
/// C's platform macros (`_WIN32`, `__linux__`, `__APPLE__`, `__x86_64__`, …). Each
/// is an object-like macro defined to `1`, seeded into the preprocessor so a
/// program can `#ifdef _WIN32` / `#ifdef __linux__` and select per target — the
/// mechanism a platform header like `<windows.hc>` is gated on.
///
/// `__HCC__` (the compiler marker, C analog `__GNUC__`) is always defined. An
/// unrecognized triple yields just that marker. This is the single source of truth
/// for the policy; the CLI's `Target` and the integration tests both consult it.
pub fn target_macros(triple: &str) -> Vec<(&'static str, &'static str)> {
    let mut m = vec![("__HCC__", "1")];
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
/// macros; an unrecognized host yields `""` (the `__HCC__`-only macro set).
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
