//! Native code generators behind the [`Codegen`] trait. A codegen backend is a
//! **target**, not just an architecture — its module is named `<arch>_<os>`
//! because the object format, syscalls, and ABI all depend on the OS, not only
//! the CPU. Two implement the trait: [`arm64`] (AArch64 encoder + a Mach-O
//! container linked via `cc`; `aarch64-apple-darwin`) and [`x86_64`] (the shared
//! x86-64 codegen with a
//! per-OS policy; its `X64Linux` target is a freestanding static ELF with raw
//! Linux syscalls, `x86_64-unknown-linux`, and a Windows PE target is underway).
//! (The tree-walking
//! [interpreter](crate::interp) is not a code generator and lives separately; it
//! is the conformance oracle these backends match byte-for-byte.)

pub mod arm64;
pub mod x86_64;

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
