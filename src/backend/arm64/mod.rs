//! The AArch64 code-generation backends and their OS seam.
//!
//! This module is the shared frame for both AArch64 targets: the [`Codegen`] impls
//! `Arm64Darwin` (a Mach-O relocatable object linked with the system `cc`) and
//! `Arm64Linux` (a freestanding static ELF), the [`ArmTarget`] trait that captures the
//! only per-OS difference (a relocatable object + linker vs. a self-contained executable),
//! the register-numbering constants, and the `build` driver. The instruction selection
//! itself — walking the SSA [IR](crate::ir) after [`crate::backend::destruct_program`] and
//! emitting AArch64 — lives in the `isel` submodule (see its doc for the spill-everything +
//! register-promotion model, switch tables, and the exception ABI); the encoder and its
//! peephole live in `asm`. Both backends match the IR interpreter (the conformance oracle)
//! byte-for-byte; see `tests/arm64_darwin.rs`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::ast::Program;
use crate::backend::{Codegen, CodegenError};

mod asm;
mod darwin;
mod isel;
mod linux;

pub use linux::Arm64Linux;

use asm::CodeImage;

const RES: u32 = 9; // integer/pointer expression result
const T2: u32 = 10; // secondary integer temporary
const SCRATCH: u32 = 8; // scratch (e.g. `%` quotient, strides, fp<->gpr conduit)
const FP: u32 = 29;
const LR: u32 = 30;
const SP: u32 = 31;

const XZR: u32 = 31;

// Per-instruction register-liveness tags for the peephole pass (`Asm`).
// `inst_use` is a bitmask over the general-purpose registers x0–x30, where bit r
// means xr; x31 (SP/XZR) is never tracked. `inst_branch` classifies control flow.
const GP_ALL: u32 = 0x7FFF_FFFF; // x0..x30 (conservative "reads everything")
const B_NORMAL: u8 = 0; // straight-line instruction
const B_CALL: u8 = 1; // bl/blr — clobbers the caller-saved temporaries
const B_RET: u8 = 2; // ret — only the return value / callee-saved are live-out
const B_BRANCH: u8 = 3; // any other branch — a barrier for the liveness scan

/// Bit for GP register `r` in an `inst_use` mask (x31 = SP/XZR is not tracked).
fn gpb(r: u32) -> u32 {
    if r < 31 { 1 << r } else { 0 }
}

pub struct Arm64Darwin {
    out_path: PathBuf,
}

/// Per-OS object format and link policy. The AArch64 instruction encoding and the
/// code generation are shared between targets. This trait captures the only
/// Darwin-vs-Linux difference: the relocatable-object container (Mach-O vs ELF, each
/// with its own relocation types and symbol-name conventions) and the linker.
trait ArmTarget {
    /// Package the machine code and symbolic relocations into a relocatable object.
    /// `defined` are the `_main` and function symbols with their `__text` byte
    /// offsets, `commons` the BSS-allocated globals, and `ndefined` the count of
    /// defined symbols. Only hosted targets (Darwin) implement this. A
    /// [`freestanding`](ArmTarget::freestanding) target instead emits an executable
    /// directly via [`write_executable`](ArmTarget::write_executable).
    fn write_object(
        &self,
        _image: &CodeImage,
        _defined: &[(String, u64)],
        _commons: &[(String, u64, u32)],
        _ndefined: u32,
    ) -> Vec<u8> {
        unreachable!("write_object is only called for hosted (non-freestanding) targets")
    }

    /// Link the relocatable object `obj` into the executable `out`. Only hosted
    /// targets implement this (Darwin, via `cc`); freestanding targets need no
    /// linker.
    fn link(&self, _obj: &Path, _out: &Path) -> Result<(), CodegenError> {
        unreachable!("link is only called for hosted (non-freestanding) targets")
    }

    /// `true` for a freestanding target: one that emits a self-contained static
    /// executable with its own `_start` and raw syscalls, calling no libc and needing
    /// no linker (`aarch64-unknown-linux` with no C toolchain). When set, the driver
    /// emits a `_start` entry and `compile` returns the finished executable from
    /// [`write_executable`](ArmTarget::write_executable) rather than a relocatable
    /// object. The hosted Darwin target leaves this `false` and instead uses
    /// [`write_object`](ArmTarget::write_object) plus `link` (via `cc`).
    fn freestanding(&self) -> bool {
        false
    }

    /// Wrap the freestanding `code` into a runnable executable. The entry is the
    /// first byte of `code`, and `bss` zero bytes trail the image. Only called when
    /// [`freestanding`](ArmTarget::freestanding) is `true`.
    fn write_executable(&self, _code: &[u8], _bss: u64) -> Vec<u8> {
        unreachable!("write_executable is only called for freestanding targets")
    }
}

impl Arm64Darwin {
    pub fn new(out_path: impl Into<PathBuf>) -> Self {
        Arm64Darwin {
            out_path: out_path.into(),
        }
    }

    /// Emit the Mach-O relocatable object for `program` as raw bytes, without
    /// linking. Exposed so structural tests can byte-check the object on any host.
    pub fn object(&self, program: &Program) -> Result<Vec<u8>, CodegenError> {
        let ir = crate::backend::lower_to_machine_ir(program)?;
        isel::compile_ir(&ir, &darwin::Darwin)
    }
}

fn build(program: &Program, out_path: &Path, target: &dyn ArmTarget) -> Result<(), CodegenError> {
    // The IR-driven backend (lower → SSA IR → AArch64) is the arm64 code generator.
    let ir = crate::backend::lower_to_machine_ir(program)?;
    let obj = isel::compile_ir(&ir, target)?;
    if target.freestanding() {
        fs::write(out_path, &obj)
            .map_err(|e| CodegenError::new(format!("cannot write executable: {e}"), None))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(out_path, fs::Permissions::from_mode(0o755));
        }
        return Ok(());
    }
    static OBJ_SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = OBJ_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = std::env::temp_dir().join(format!("solomon-{}-{seq}.o", std::process::id()));
    fs::write(&tmp, &obj)
        .map_err(|e| CodegenError::new(format!("cannot write object file: {e}"), None))?;
    let result = target.link(&tmp, out_path);
    let _ = fs::remove_file(&tmp);
    result
}

impl Codegen for Arm64Darwin {
    fn name(&self) -> &'static str {
        "aarch64-apple-darwin"
    }

    fn run(&mut self, program: &Program) -> Result<(), CodegenError> {
        build(program, &self.out_path, &darwin::Darwin)
    }
}
