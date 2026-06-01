//! The `x86_64-pc-windows` target: `kernel32` imports and a self-contained PE
//! executable.
//!
//! Work in progress. The x86-64 code generation is shared with the Linux target
//! through the parent module's [`OsTarget`] seam; what remains for Windows is the
//! policy itself — exit / page allocation / the stdout sink lowered to
//! `ExitProcess` / `VirtualAlloc` / `GetStdHandle`+`WriteFile` calls through the
//! import address table — and the container: a hand-built PE with an import
//! directory (no linker, like the Linux ELF), so the `.exe` runs under Wine for
//! conformance testing. Until that lands, [`X64Windows::run`] reports a clear
//! "not yet implemented" error.

use std::path::PathBuf;

use crate::ast::Program;
use crate::codegen::{Codegen, CodegenError};

/// Compiles a HolyC program to a self-contained PE executable for `x86_64-pc-windows`.
pub struct X64Windows {
    out_path: PathBuf,
}

impl X64Windows {
    pub fn new(out_path: impl Into<PathBuf>) -> Self {
        X64Windows {
            out_path: out_path.into(),
        }
    }
}

impl Codegen for X64Windows {
    fn name(&self) -> &'static str {
        "x86_64-pc-windows"
    }

    fn run(&mut self, _program: &Program) -> Result<(), CodegenError> {
        Err(CodegenError::new(
            format!(
                "x86_64-pc-windows backend not yet implemented \
                 (PE container + kernel32 imports in progress); target was {}",
                self.out_path.display()
            ),
            None,
        ))
    }
}
