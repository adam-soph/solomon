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
pub mod builtins;
pub mod codegen;
pub mod fmt;
pub mod interp;
pub mod layout;
pub mod lexer;
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
    let mut dirs = Vec::new();
    if let Ok(env) = std::env::var("SOLOMON_STDLIB") {
        dirs.extend(env.split(':').filter(|s| !s.is_empty()).map(PathBuf::from));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(d) = exe.parent() {
            dirs.push(d.join("lib"));
            dirs.push(d.join("../lib"));
            dirs.push(d.join("../../lib"));
        }
    }
    dirs.push(PathBuf::from("lib"));
    dirs
}
