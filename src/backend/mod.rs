//! The native-codegen layer shared by every backend.
//!
//! This module owns three things:
//!
//! 1. **The outer interface.** [`Codegen`] is the per-target trait (`name` + `run`) that each
//!    backend — [`Arm64Darwin`](crate::Arm64Darwin), [`Arm64Linux`](crate::Arm64Linux),
//!    [`X64Linux`](crate::X64Linux), [`X64Windows`](crate::X64Windows) — implements to lower
//!    a type-checked [`Program`] to a binary. [`CodegenError`] is the error these (and the IR
//!    [interpreter](crate::irinterp)) raise.
//!
//! 2. **The IR-level shared support** for the two machine-code generators
//!    (`crate::backend::arm64::isel` and `crate::backend::x86_64::isel`). Both consume the *one* SSA
//!    [IR](crate::ir): they walk a function's `phi`-free blocks and select machine
//!    instructions. The instruction selection, ABI, encoders, and container format are
//!    irreducibly per-architecture and live in each `isel` module. What is genuinely
//!    identical — the pure-IR analyses and the block-walk control structure — lives here, so
//!    the two backends cannot drift on it:
//!
//!    * [`reachable_functions`] / [`heap_prims_used`] / [`func_uses_fs`] — pure scans over
//!      the IR, no machine state.
//!    * the [`Backend`] trait + [`emit_blocks`] driver — the per-function "for each block:
//!      place its label, emit its instructions, emit its terminator" loop. Each `isel` does
//!      its own prologue/frame setup, calls [`emit_blocks`], then its own epilogue/patch.
//!
//! 3. **The out-of-SSA + register-promotion pass.** [`destruct_program`] resolves the IR's
//!    `phi` nodes into ordinary register copies on the CFG edges (critical-edge splitting +
//!    parallel-copy sequencing), yielding the `phi`-free form a backend lowers block by
//!    block; [`plan_registers`] then runs a liveness-based linear scan that promotes hot
//!    vregs into the target's callee-saved registers. Both are architecture-neutral IR→IR
//!    work the two backends share and the IR [interpreter](crate::irinterp) skips entirely
//!    (it runs the SSA form directly), so they live here alongside the rest of the shared
//!    backend support rather than in either `isel`.

use std::collections::{HashMap, HashSet};
use std::fmt;

use crate::ast::Program;
use crate::ir::*;
use crate::token::Pos;

mod analysis;
pub mod arm64;
mod licm;
mod regalloc;
mod simplify;
pub mod x86_64;

use regalloc::{Location, RegSet, allocate};

/// An error raised while running a program.
///
/// This is either a runtime fault in the interpreter or an emission failure in a
/// codegen backend. It carries a source position when one is available.
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

    /// Compiles the program, which is already parsed and type-checked, and writes
    /// the output binary. Linking and file I/O are the backend's own concern.
    fn run(&mut self, program: &Program) -> Result<(), CodegenError>;
}

// ---- shared runtime-policy constants ----
//
// Tuning values the two backends' impure-primitive runtimes must agree on. Each backend
// emits its own (arch-specific) instructions, but a one-sided change to one of these numbers
// would make the backends behave subtly differently, so they are defined once here. (Truly
// arch/OS-specific values — `clone(2)` flags, syscall numbers, mmap `PROT`/`MAP` bits — stay
// in each `isel`, since they legitimately differ.) Each backend casts to its own width.

/// Safety-net timeout for a futex wait, in nanoseconds (~1 ms). A missed wakeup degrades to
/// a re-check at this period instead of deadlocking; never reached when wakeups work.
pub const FUTEX_TIMEOUT_NS: u64 = 1_000_000;

/// Stack (plus thread control block) for a freestanding `clone(2)`-spawned thread: 128 KiB.
pub const THREAD_STACK_SIZE: u64 = 0x2_0000;

/// The freestanding bump allocator's chunk grab: each fresh `mmap` is `max(request, this)`,
/// rounded up to a page. 1 MiB.
pub const HEAP_CHUNK_SIZE: u64 = 0x10_0000;

/// Lower a type-checked AST `program` to the phi-free, out-of-SSA [`IrProgram`] that both
/// native backends consume: `layout::compute` → `lower` → [`destruct_program`],
/// surfacing the first layout error as a `CodegenError` (the interpreter path does the
/// same in `irinterp::run_to_string_with_input`, so all three consumers reject the same
/// programs). This is the entire pre-encoding pipeline; each backend's `compile_ir` takes
/// the result, so the lowering lives here once instead of being copied into every backend
/// (where the layout errors were also being silently dropped).
pub fn lower_to_machine_ir(program: &crate::ast::Program) -> Result<IrProgram, CodegenError> {
    let (layouts, layout_errs) = crate::layout::compute(program);
    if let Some(e) = layout_errs.into_iter().next() {
        return Err(CodegenError::at(
            e.pos,
            format!("layout error: {}", e.message),
        ));
    }
    let ir = crate::lower::lower(program, &layouts)?;
    // Scalar simplification (constant folding, algebraic identities, power-of-2 strength
    // reduction) on the shared IR — backend path only; the interpreter keeps the unoptimized
    // IR as the oracle. Behavior-preserving: folds run the oracle's own arithmetic.
    let ir = simplify::run(ir);
    debug_assert!(
        crate::ir::verify(&ir).is_empty(),
        "simplify produced invalid IR: {:?}",
        crate::ir::verify(&ir)
    );
    // Loop-invariant code motion on the shared IR (backend path only; the interpreter keeps
    // the unoptimized IR as the oracle). Semantics-preserving — validated by the goldens.
    let ir = licm::run(ir);
    debug_assert!(
        crate::ir::verify(&ir).is_empty(),
        "LICM produced invalid IR: {:?}",
        crate::ir::verify(&ir)
    );
    Ok(destruct_program(&ir))
}

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

/// Whether any function in the program establishes or raises an exception (`try`/`throw`).
/// Register promotion must be disabled program-wide when this is true: a `throw`'s longjmp
/// abandons intermediate frames without restoring their callee-saved registers, so any
/// promoting frame on the stack during an unwind would corrupt its caller's registers (see
/// [`crate::backend::plan_registers`]).
pub fn program_has_exceptions(ir: &IrProgram) -> bool {
    ir.funcs.iter().any(|f| {
        f.blocks.iter().any(|b| {
            matches!(b.term, IrTerm::Throw(_) | IrTerm::Rethrow)
                || b.insts
                    .iter()
                    .any(|i| matches!(i, IrInst::TryBegin { .. } | IrInst::TryEnd))
        })
    })
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

/// Whether any reachable function actually touches the per-task `Fs` (so the `CTask`/per-task
/// setup is needed). `fs` is the `Fs` global's id, or `None` if the program never registered
/// it — both backends gate the same `CTask` machinery on this.
pub fn prog_uses_fs(reachable: &[&IrFunc], fs: Option<GlobalId>) -> bool {
    fs.is_some_and(|g| reachable.iter().any(|f| func_uses_fs(f, g)))
}

/// The ids of the implicit globals the front end injects — `Fs`, `argc`, `argv`, `envp` —
/// each `None` if the program did not register it (a global's id is its index in
/// `ir.globals`). Both backends resolve these identically, so it lives here.
pub struct ImplicitGlobals {
    pub fs: Option<GlobalId>,
    pub argc: Option<GlobalId>,
    pub argv: Option<GlobalId>,
    pub envp: Option<GlobalId>,
}

pub fn implicit_globals(ir: &IrProgram) -> ImplicitGlobals {
    let gid = |name: &str| {
        ir.globals
            .iter()
            .position(|g| g.name == name)
            .map(|i| i as GlobalId)
    };
    ImplicitGlobals {
        fs: gid("Fs"),
        argc: gid("argc"),
        argv: gid("argv"),
        envp: gid("envp"),
    }
}

/// The `CTask` field offsets the exception unwinder reads — `exc_top` (the handler-frame
/// chain head) and `except_ch` (the thrown value) — plus the task's size, all in raw bytes
/// (each backend casts to its own offset width). `CTask` comes from the implicit prelude, so
/// it is always laid out; an `Err` means it was redefined without a required field.
pub struct CTaskLayout {
    pub exc_top: u64,
    pub except_ch: u64,
    pub size: u64,
}

pub fn ctask_layout(ir: &IrProgram) -> Result<CTaskLayout, CodegenError> {
    let field = |name: &str| {
        ir.layouts.offset_of("CTask", name).ok_or_else(|| {
            CodegenError::new(format!("IR backend: `CTask` has no field `{name}`"), None)
        })
    };
    Ok(CTaskLayout {
        exc_top: field("exc_top")?,
        except_ch: field("except_ch")?,
        size: ir
            .layouts
            .size_of(&crate::ast::Type::Named("CTask".to_string()))
            .max(8),
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

// ===========================================================================
// Out-of-SSA destruction + register promotion
//
// The SSA->machine-IR pass both native backends run and the interpreter skips:
// `destruct_program` strips `phi` nodes into edge copies, `plan_registers`
// promotes hot vregs into callee-saved registers. Architecture-neutral IR->IR
// work, shared here so the two backends consume one identical phi-free form.
// ===========================================================================

/// Resolve all `phi` nodes in `f`, returning an equivalent `phi`-free function.
pub fn destruct_ssa(f: &IrFunc) -> IrFunc {
    let mut blocks: Vec<IrBlock> = f.blocks.clone();
    let mut next_vreg = f.n_vregs;

    // Predecessors, from the original terminators (before any edge splitting).
    let mut preds: Vec<Vec<BlockId>> = vec![Vec::new(); blocks.len()];
    for b in &blocks {
        for s in b.term.successors() {
            preds[s as usize].push(b.id);
        }
    }

    let mut split_blocks: Vec<IrBlock> = Vec::new();
    for b_id in 0..blocks.len() {
        let phis = std::mem::take(&mut blocks[b_id].phis);
        if phis.is_empty() {
            continue;
        }
        for a in preds[b_id].clone() {
            // The parallel copies for edge A → B: each phi's destination takes the
            // value the phi names for predecessor A.
            let copies: Vec<(Vreg, IrTy, Val)> = phis
                .iter()
                .map(|phi| {
                    let val = phi
                        .args
                        .iter()
                        .find(|(p, _)| *p == a)
                        .map(|(_, v)| *v)
                        .unwrap_or(Val::ImmInt(0));
                    (phi.dst, phi.ty, val)
                })
                .collect();
            let movs = sequence_copies(&copies, &mut next_vreg);

            if blocks[a as usize].term.successors().len() == 1 {
                // The only edge out of A is A → B: place copies at the end of A.
                blocks[a as usize].insts.extend(movs);
            } else {
                // Critical edge: split it with a new block carrying the copies.
                let s_id = (blocks.len() + split_blocks.len()) as BlockId;
                split_blocks.push(IrBlock {
                    id: s_id,
                    phis: Vec::new(),
                    insts: movs,
                    term: IrTerm::Br(b_id as BlockId),
                });
                redirect_term(&mut blocks[a as usize].term, b_id as BlockId, s_id);
            }
        }
    }
    blocks.extend(split_blocks);

    IrFunc {
        name: f.name.clone(),
        ret: f.ret,
        params: f.params.clone(),
        varargs: f.varargs,
        slots: f.slots.clone(),
        blocks,
        entry: f.entry,
        n_vregs: next_vreg,
    }
}

/// Out-of-SSA every function in a program.
pub fn destruct_program(p: &IrProgram) -> IrProgram {
    IrProgram {
        funcs: p.funcs.iter().map(destruct_ssa).collect(),
        globals: p.globals.clone(),
        strings: p.strings.clone(),
        layouts: p.layouts.clone(),
    }
}

/// Sequence a set of parallel copies (`dst_i = src_i`, all conceptually simultaneous) into
/// `Mov`s (copy coalescing, T4c). The destinations are distinct (one per `phi`). A copy `i`
/// whose source is `dst_j` reads `j`'s *old* value, so `i` must run before `j` overwrites it —
/// an edge `i → j`. With no cycle among these constraints the copies serialize in topological
/// order as **direct** moves (no temporaries — so a loop-carried value stops round-tripping a
/// spill slot every back-edge). A cycle (a swap `a=b; b=a`, or longer) genuinely needs a
/// temporary, so that case falls back to the always-correct route-through-fresh-temps form.
fn sequence_copies(copies: &[(Vreg, IrTy, Val)], next_vreg: &mut u32) -> Vec<IrInst> {
    // Drop no-op self-copies (`dst = dst`); they constrain nothing and need no move.
    let copies: Vec<(Vreg, IrTy, Val)> = copies
        .iter()
        .filter(|(d, _, s)| *s != Val::Reg(*d))
        .copied()
        .collect();
    if copies.is_empty() {
        return Vec::new();
    }

    let dst_idx: HashMap<Vreg, usize> = copies
        .iter()
        .enumerate()
        .map(|(i, (d, _, _))| (*d, i))
        .collect();
    // Edge `i → j` (i before j) when copy i's source is copy j's destination.
    let n = copies.len();
    let mut succ: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut indeg = vec![0u32; n];
    for (i, (_, _, src)) in copies.iter().enumerate() {
        if let Val::Reg(s) = src {
            if let Some(&j) = dst_idx.get(s) {
                if j != i {
                    succ[i].push(j);
                    indeg[j] += 1;
                }
            }
        }
    }
    // Kahn's topological sort over the precedence constraints.
    let mut order: Vec<usize> = (0..n).filter(|&i| indeg[i] == 0).collect();
    let mut qi = 0;
    while qi < order.len() {
        let i = order[qi];
        qi += 1;
        for k in 0..succ[i].len() {
            let j = succ[i][k];
            indeg[j] -= 1;
            if indeg[j] == 0 {
                order.push(j);
            }
        }
    }

    if order.len() == n {
        // Acyclic: emit direct moves in dependency order — no temporaries.
        return order
            .iter()
            .map(|&i| {
                let (dst, ty, src) = copies[i];
                IrInst::Mov { dst, ty, src }
            })
            .collect();
    }

    // A cycle remains: route every source through a fresh temporary, then write every
    // destination from its temporary. Correct for any dependency pattern (swaps, cycles).
    let mut movs = Vec::with_capacity(copies.len() * 2);
    let mut temps = Vec::with_capacity(copies.len());
    for (_, ty, src) in &copies {
        let t = *next_vreg;
        *next_vreg += 1;
        movs.push(IrInst::Mov {
            dst: t,
            ty: *ty,
            src: *src,
        });
        temps.push(t);
    }
    for (i, (dst, ty, _)) in copies.iter().enumerate() {
        movs.push(IrInst::Mov {
            dst: *dst,
            ty: *ty,
            src: Val::Reg(temps[i]),
        });
    }
    movs
}

/// Redirect every `from` successor of a terminator to `to` (for edge splitting).
fn redirect_term(term: &mut IrTerm, from: BlockId, to: BlockId) {
    let swap = |b: &mut BlockId| {
        if *b == from {
            *b = to;
        }
    };
    match term {
        IrTerm::Br(b) => swap(b),
        IrTerm::CondBr { t, f, .. } => {
            swap(t);
            swap(f);
        }
        IrTerm::Switch { cases, default, .. } => {
            for (_, _, b) in cases {
                swap(b);
            }
            swap(default);
        }
        IrTerm::Ret(_) | IrTerm::Throw(_) | IrTerm::Rethrow | IrTerm::Unreachable => {}
    }
}

/// A physical register the allocator hands a vreg ([`Location::Reg`]): an opaque register
/// number from the pool the backend passed to [`allocate`], interpreted by that backend
/// (arm64 x-/v-registers, x86-64 GPRs/xmm), tagged with its register class.
#[derive(Clone, Copy)]
pub struct PReg {
    pub is_float: bool,
    pub num: u32,
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
