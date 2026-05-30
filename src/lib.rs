//! Solomon: a reimplementation of HolyC.
//!
//! This chunk provides the lexer and parser. Later chunks (type checker,
//! codegen) build on the AST produced here.

pub mod ast;
pub mod backend;
pub mod builtins;
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
