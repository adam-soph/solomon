//! Shared IR-level backend support for the native machine-code generators
//! ([`crate::arm64::isel`] and [`crate::x86_64::isel`]).
//!
//! Both backends consume the *one* SSA [IR](crate::ir): they walk a function's `phi`-free
//! blocks and select machine instructions. The instruction selection, ABI, encoders, and
//! container format are irreducibly per-architecture and live in each `isel` module. What
//! is genuinely identical — the pure-IR analyses and the block-walk control structure —
//! lives here, so the two backends cannot drift on it:
//!
//! * [`reachable_functions`] / [`heap_prims_used`] / [`func_uses_fs`] — pure scans over the
//!   IR, no machine state.
//! * the [`Backend`] trait + [`emit_blocks`] driver — the per-function "for each block:
//!   place its label, emit its instructions, emit its terminator" loop. Each `isel` does
//!   its own prologue/frame setup, calls [`emit_blocks`], then its own epilogue/patch.

use std::collections::{HashMap, HashSet};

use crate::codegen::CodegenError;
use crate::ir::*;

/// The functions reachable from `@entry` over direct calls and `&Func`, with `@entry`
/// first (the x86 backend needs it as the program entry point; harmless for arm64, which
/// reaches every function by label). Errors if a called function was never lowered (e.g. a
/// nested function the front end dropped), tagged with the caller's `backend` label.
pub fn reachable_functions<'a>(
    ir: &'a IrProgram,
    backend: &str,
) -> Result<Vec<&'a IrFunc>, CodegenError> {
    let by_name: HashMap<&str, &IrFunc> = ir.funcs.iter().map(|f| (f.name.as_str(), f)).collect();
    let mut reachable: Vec<&IrFunc> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    let mut queue: Vec<&str> = Vec::new();
    if by_name.contains_key(crate::lower::ENTRY) {
        queue.push(crate::lower::ENTRY);
    }
    while let Some(name) = queue.pop() {
        if !seen.insert(name) {
            continue;
        }
        let f = by_name.get(name).ok_or_else(|| {
            CodegenError::new(
                format!("IR {backend}: needed function `{name}` was not lowered"),
                None,
            )
        })?;
        reachable.push(f);
        for b in &f.blocks {
            for inst in &b.insts {
                match inst {
                    IrInst::Call {
                        callee: Callee::Direct(n),
                        ..
                    } => queue.push(n),
                    IrInst::FuncAddr { func, .. } => queue.push(func),
                    _ => {}
                }
            }
        }
    }
    reachable.sort_by_key(|f| f.name != crate::lower::ENTRY);
    Ok(reachable)
}

/// Which heap primitives the given functions call (the freestanding/bump heap runtime
/// emits exactly these; `MSize` makes `MAlloc`/`HeapExtend` carry a size header).
pub fn heap_prims_used(funcs: &[&IrFunc]) -> HashSet<&'static str> {
    let mut used = HashSet::new();
    for f in funcs {
        for b in &f.blocks {
            for inst in &b.insts {
                if let IrInst::Prim { prim, .. } = inst {
                    match prim {
                        Prim::MAlloc => used.insert("MAlloc"),
                        Prim::Free => used.insert("Free"),
                        Prim::HeapExtend => used.insert("HeapExtend"),
                        Prim::MSize => used.insert("MSize"),
                        _ => false,
                    };
                }
            }
        }
    }
    used
}

/// Whether `f` touches the per-task `Fs` — it accesses the `Fs` global (`Fs->field`) or
/// has any exception op (`try`/`throw`). Such a function needs the `CTask`/exception setup.
pub fn func_uses_fs(f: &IrFunc, fs_gid: GlobalId) -> bool {
    f.blocks.iter().any(|b| {
        matches!(b.term, IrTerm::Throw(_) | IrTerm::Rethrow)
            || b.insts.iter().any(|i| match i {
                IrInst::TryBegin { .. } | IrInst::TryEnd => true,
                IrInst::GlobalAddr { global, .. } => *global == fs_gid,
                _ => false,
            })
    })
}

/// A per-architecture machine-code generator, driven block by block by [`emit_blocks`].
/// The implementor (each `isel`'s per-function emitter) holds the `Asm`, the block labels,
/// and the value-access state; these three hooks are the leaf instruction selection.
pub trait Backend {
    /// Place the label for the `block_index`-th block of the function being emitted.
    fn place_block(&mut self, block_index: usize);
    /// Select machine instructions for one IR instruction.
    fn emit_inst(&mut self, inst: &IrInst) -> Result<(), CodegenError>;
    /// Select machine instructions for a block terminator (branch / return / unwind).
    fn emit_term(&mut self, term: &IrTerm) -> Result<(), CodegenError>;
}

/// Walk an out-of-SSA function's blocks in order, emitting each block's label, its
/// instructions, then its terminator. The caller wraps this with its own per-arch prologue
/// and epilogue/frame-patch.
pub fn emit_blocks<B: Backend>(b: &mut B, f: &IrFunc) -> Result<(), CodegenError> {
    for (i, blk) in f.blocks.iter().enumerate() {
        b.place_block(i);
        for inst in &blk.insts {
            b.emit_inst(inst)?;
        }
        b.emit_term(&blk.term)?;
    }
    Ok(())
}
