//! The [`Codegen`] trait and [`CodegenError`] — the shared interface every native
//! code generator implements. The generators themselves are per-architecture
//! sibling modules: [`crate::arm64`] (AArch64 — `aarch64-apple-darwin` Mach-O via
//! `cc`, and `aarch64-unknown-linux-{gnu,musl}` ELF via gcc) and [`crate::x86_64`]
//! (`x86_64-unknown-linux` freestanding ELF, `x86_64-pc-windows` PE). A backend is
//! a **target** — an (architecture, OS) pair — since the object format, syscalls,
//! and ABI depend on the OS, not just the CPU. (The tree-walking
//! [interpreter](crate::interp) is not a code generator and lives separately; it
//! is the conformance oracle these backends match byte-for-byte.)

use std::fmt;

use crate::ast::Program;
use crate::token::Pos;

/// An error raised while running a program: a runtime fault in the interpreter or
/// an emission failure in a codegen backend. Carries a source position when one
/// is available.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodegenError {
    pub message: String,
    pub pos: Option<Pos>,
}

impl CodegenError {
    pub fn new(message: impl Into<String>, pos: Option<Pos>) -> Self {
        CodegenError {
            message: message.into(),
            pos,
        }
    }

    /// An error located at a specific source position.
    pub fn at(pos: Pos, message: impl Into<String>) -> Self {
        CodegenError::new(message, Some(pos))
    }
}

impl fmt::Display for CodegenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.pos {
            Some(pos) => write!(f, "runtime error at {pos}: {}", self.message),
            None => write!(f, "runtime error: {}", self.message),
        }
    }
}

impl std::error::Error for CodegenError {}

/// A native code-generation backend: lowers a program to a binary for one target.
pub trait Codegen {
    /// The target triple this backend emits for, e.g. `"x86_64-unknown-linux"`.
    fn name(&self) -> &'static str;

    /// Compile the (already parsed and type-checked) program, writing the output
    /// binary. Linking and file I/O are the backend's own concern.
    fn run(&mut self, program: &Program) -> Result<(), CodegenError>;
}
