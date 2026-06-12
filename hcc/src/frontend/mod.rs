//! The compiler front end: source text → a fully-concrete, type-checked
//! [`Program`](crate::ast::Program) plus its [`Layouts`](crate::layout::Layouts).
//!
//! These are the passes that run before lowering. They form a linear pipeline, each a
//! submodule here:
//!
//! 1. [`lexer`] — streams source bytes into [`Token`](crate::token::Token)s. Tokens are
//!    *never* materialized into a list (see the [`TokenStream`](crate::lexer::TokenStream)
//!    trait); everything downstream pulls lazily.
//! 2. [`preproc`] — the [`Preprocessor`](crate::preproc::Preprocessor), a `TokenStream`
//!    wrapping the lexer: `#include` (stacking lexers), object/function macros, and
//!    `#if`/`#ifdef` conditionals.
//! 3. [`parser`] — pulls the preprocessed token stream into the typed AST. Two-pass
//!    (`hoist_type_names` then the real parse), and it *defers* every generic use to an AST
//!    node rather than instantiating.
//! 4. [`mono`] — monomorphizes those deferred generics into concrete AST, so everything
//!    after it sees an ordinary, fully-concrete `Program`.
//! 5. [`sema`] — name resolution, visibility, and type inference; annotates every
//!    expression's [`ty`](crate::ast::Expr::ty) via interior mutability.
//! 6. [`layout`] — `repr(C)` sizes, offsets, and strides ([`Layouts`](crate::layout::Layouts)),
//!    consumed by lowering, the interpreter, and both backends.
//!
//! The shared data types these passes produce — [`token`](crate::token) and
//! [`ast`](crate::ast) — live at the crate root, since the IR, interpreter, and backends
//! consume them too. The boundary out of the front end is `crate::lower`, which turns the
//! type-checked AST into the SSA [IR](crate::ir).

pub mod layout;
pub mod lexer;
pub mod lower;
pub mod mono;
pub mod parser;
pub mod preproc;
pub mod sema;
