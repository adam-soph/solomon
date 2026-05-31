//! Solomon: a reimplementation of HolyC.
//!
//! A full compiler front end — lexer, preprocessor, parser, semantic analysis,
//! and type layout — feeding two backends behind the [`Backend`] trait: a
//! tree-walking interpreter and a hand-rolled AArch64 native code generator. The
//! interpreter is the conformance oracle the native backend matches byte-for-byte.

pub mod ast;
pub mod backend;
pub mod builtins;
pub mod fmt;
pub mod layout;
pub mod lexer;
pub mod parser;
pub mod preproc;
pub mod sema;
pub mod token;

pub use ast::{Expr, ExprKind, Program, Stmt, StmtKind, Type};
pub use backend::arm64::Arm64;
pub use backend::interp::Interpreter;
pub use backend::{Backend, BackendError};
pub use layout::{Layout, Layouts};
pub use lexer::{LexError, Lexer, TokenStream, tokenize};
pub use parser::{ParseError, Parser, parse};
pub use preproc::Preprocessor;
pub use sema::{SemaError, analyze, check_program};
pub use token::{Keyword, Pos, Span, Token, TokenKind};
