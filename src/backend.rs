//! Backends consume a type-checked [`Program`] and do something with it.
//!
//! The tree-walking [`interp`] reter is the first backend; future backends (a
//! bytecode VM, C-source emission, native codegen, …) implement the same
//! [`Backend`] trait. Construction is backend-specific — an interpreter is built
//! with an output sink, a codegen backend with somewhere to emit code — but they
//! are all driven uniformly through [`Backend::run`].

pub mod arm64;
pub mod interp;

use std::fmt;

use crate::ast::Program;
use crate::token::Pos;

/// An error raised while a backend runs (a runtime fault in the interpreter, an
/// emission failure in a codegen backend, …). Carries a source position when
/// one is available.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendError {
    pub message: String,
    pub pos: Option<Pos>,
}

impl BackendError {
    pub fn new(message: impl Into<String>, pos: Option<Pos>) -> Self {
        BackendError {
            message: message.into(),
            pos,
        }
    }

    /// An error located at a specific source position.
    pub fn at(pos: Pos, message: impl Into<String>) -> Self {
        BackendError::new(message, Some(pos))
    }
}

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.pos {
            Some(pos) => write!(f, "runtime error at {pos}: {}", self.message),
            None => write!(f, "runtime error: {}", self.message),
        }
    }
}

impl std::error::Error for BackendError {}

/// A solomon backend: something that executes or translates a program.
pub trait Backend {
    /// A short identifier, e.g. `"interp"`.
    fn name(&self) -> &'static str;

    /// Process the (already parsed and ideally type-checked) program. Side
    /// effects — printing, writing files — are the backend's own concern.
    fn run(&mut self, program: &Program) -> Result<(), BackendError>;
}
