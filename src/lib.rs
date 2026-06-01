//! Solomon: a reimplementation of HolyC.
//!
//! A full compiler front end — lexer, preprocessor, parser, semantic analysis,
//! and type layout — feeding a tree-walking [`Interpreter`] (the conformance
//! oracle) and two native code generators behind the [`Codegen`] trait, each
//! named for its target: [`Arm64Darwin`] (`aarch64-apple-darwin`) and
//! [`X64Linux`] (`x86_64-unknown-linux`). The native backends match the
//! interpreter byte-for-byte.

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

pub use ast::{Expr, ExprKind, Program, Stmt, StmtKind, Type};
pub use codegen::arm64::{Arm64Darwin, Arm64Linux};
pub use codegen::x86_64::{X64Linux, X64Windows};
pub use codegen::{Codegen, CodegenError};
pub use interp::Interpreter;
pub use layout::{Layout, Layouts};
pub use lexer::{LexError, Lexer, TokenStream, tokenize};
pub use parser::{ParseError, Parser, parse};
pub use preproc::Preprocessor;
pub use sema::{SemaError, analyze, check_program};
pub use token::{Keyword, Pos, Span, Token, TokenKind};
