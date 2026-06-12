//! AArch64 instruction selection — the arm64 backend's machine-code generator.
//!
//! This is **not** an IR; it *consumes* the one SSA [IR](crate::ir) (shared with the
//! interpreter and the x86 backend) and emits AArch64. The block-walk driver and the
//! pure-IR analyses are shared via [`crate::backend`]; this module supplies the per-arch
//! instruction selection, ABI, exception unwind, primitives, and `Asm` encoding.
//!
//! It lowers a program to the SSA [IR](crate::ir), destructs it out of SSA
//! ([`crate::backend::destruct_ssa`]), and emits AArch64 by walking the resulting
//! `phi`-free blocks — reusing the [`Asm`](crate::backend::arm64::asm::Asm) encoder, the Mach-O object
//! writer, and the freestanding-ELF writer. It is the sole arm64 backend for both
//! `Arm64Darwin` and `Arm64Linux` (the old AST-walking codegen is deleted); the full
//! `tests/cases/**/*.hc` conformance corpus (`tests/cases.rs`) passes through it.
//!
//! Two targets, behind one [`Ctx`]: hosted **Darwin** (a Mach-O object whose globals and
//! `CTask` are linker-resolved common symbols, primitives lowered to libc calls) and
//! freestanding **`aarch64-unknown-linux`** (a self-contained static ELF with its own
//! `_start`, globals/`CTask` at fixed BSS offsets reached by self-resolved `ADR`,
//! raw-syscall primitives, and an `mmap` bump-allocator heap — no libc, no linker).
//!
//! Register model: **spill-everything with callee-saved promotion**. By default every
//! SSA value lives in a frame slot — operands are loaded into scratch registers (GPRs,
//! or v16/v17 for F64), combined, and stored back. On top of that a liveness-based
//! linear scan ([`crate::backend::plan_registers`]) promotes hot vregs into the
//! callee-saved registers (x19–x28 / d8–d15), saved/restored in the prologue/epilogue;
//! a `try`-containing function stays fully spilled. Handles the deterministic language:
//! integers/pointers/floats, memory, control flow (incl. an O(1) jump-table `switch`),
//! direct and indirect calls, by-value aggregates (sret), globals, string literals,
//! exceptions (`try`/`catch`/`throw` via a jmp_buf/longjmp unwind over `Fs->exc_top`),
//! the command line (`argc`/`argv`/`envp`), and the pure-HolyC printf path. The impure
//! primitives are all lowered: the heap, the clock, fd/file I/O, sockets, fs mutation,
//! process ids, atomics/futex, and threads — `pthread` on Darwin, raw `clone(2)` + futex
//! join freestanding. The algebraic intrinsics `Sqrt`/`Fabs`/the rounding family lower
//! to single FP instructions in place of their lib bodies. Every example compiles and
//! matches the oracle on Darwin (verified by execution); freestanding ELFs are executed
//! on a linux/aarch64 host on CI.

use std::collections::{HashMap, HashSet};

use crate::backend::CodegenError;
use crate::backend::Location;
use crate::backend::arm64::asm::{Asm, SH_ASR, SH_LSL, SH_LSR};
use crate::backend::arm64::{ArmTarget, FP, SCRATCH, SP};
use crate::ir::*;

mod exc;
mod heap;
mod prims;
use heap::emit_heap_runtime;

// Scratch GPRs (all caller-saved; values are reloaded from slots per instruction, so
// nothing is live in them across instructions).
const TMP0: u32 = 9; // primary value scratch (RES)
const ADDR: u32 = 10; // address scratch (T2)
const TMP1: u32 = 11; // secondary value scratch
const TMP2: u32 = 12; // tertiary value scratch
const IND: u32 = 16; // indirect-call target (IP0)
const FRES: u32 = 16; // primary float scratch (v16)
const FT2: u32 = 17; // secondary float scratch (v17)
const FT3: u32 = 18; // tertiary float scratch (v18), for inline fmod

// AArch64 condition codes.
const C_EQ: u32 = 0;
const C_NE: u32 = 1;
const C_HS: u32 = 2;
const C_LO: u32 = 3;
const C_MI: u32 = 4;
const C_HI: u32 = 8;
const C_LS: u32 = 9;
const C_GE: u32 = 10;
const C_LT: u32 = 11;
const C_GT: u32 = 12;
const C_LE: u32 = 13;

/// Emit machine code from the phi-free, out-of-SSA `ir` (produced by
/// [`crate::backend::lower_to_machine_ir`]) to either a hosted Darwin Mach-O object or a
/// freestanding `aarch64-unknown-linux` static ELF, selected by `target`.
pub(super) fn compile_ir(ir: &Program, target: &dyn ArmTarget) -> Result<Vec<u8>, CodegenError> {
    // Reachable functions from `@entry`, over direct calls and `&Func` (we emit only
    // these, so the symbol table is complete).
    let reachable = crate::backend::reachable_functions(ir, "arm64 backend")?;

    // Implicit-global ids, the `CTask` layout, and Fs-use — all resolved the same way as the
    // x86 backend, so they live in `crate::backend`.
    let ig = crate::backend::implicit_globals(ir);
    let (fs_gid, argc_gid, argv_gid, envp_gid) = (ig.fs, ig.argc, ig.argv, ig.envp);
    let ctask = crate::backend::ctask_layout(ir)?;
    let (exc_top_off, except_ch_off) = (ctask.exc_top as u32, ctask.except_ch as u32);
    let ctask_size = ctask.size;
    // Whether the reachable program actually touches `Fs`/exceptions — drives the Darwin
    // per-thread pthread-TLS setup (a non-exception program needs none of it).
    let prog_uses_fs = crate::backend::prog_uses_fs(&reachable, fs_gid);
    // Exceptions disable register promotion program-wide (see `Ctx::program_has_exceptions`).
    let program_has_exceptions = crate::backend::program_has_exceptions(ir);

    // Which heap primitives the reachable code uses (the freestanding `mmap` runtime
    // emits exactly these; `MSize` makes `MAlloc`/`HeapExtend` carry a size header).
    let heap_used = crate::backend::heap_prims_used(&reachable);
    let uses_msize = heap_used.contains("MSize");

    let mut asm = Asm::new();
    let freestanding = target.freestanding();
    // The freestanding ELF entry is a `_start` at the first emitted byte.
    let start_label = if freestanding {
        Some(asm.new_label())
    } else {
        None
    };
    // One label per reachable function; `@entry` is `_main`.
    let labels: HashMap<&str, usize> = reachable
        .iter()
        .map(|f| (f.name.as_str(), asm.new_label()))
        .collect();
    // Freestanding heap-runtime entry labels, one per used routine.
    let mut heap_labels: HashMap<&'static str, usize> = HashMap::new();
    if freestanding {
        for &name in &["MAlloc", "Free", "HeapExtend", "MSize"] {
            if heap_used.contains(name) {
                heap_labels.insert(name, asm.new_label());
            }
        }
    }
    // Intern every string literal once; map IR string ids to asm string indices.
    let str_idx: Vec<usize> = ir
        .strings
        .iter()
        .map(|bytes| {
            let content = bytes.strip_suffix(&[0]).unwrap_or(bytes);
            asm.intern_string(&String::from_utf8_lossy(content))
        })
        .collect();

    let ndefined = reachable.len() as u32;

    // Build the per-target addressing context and (freestanding) the BSS layout.
    let mut bss_size = 0u64;
    let mut heap_globals: Option<(u64, u64)> = None; // (hp, he) BSS offsets
    let ctx = if freestanding {
        // Lay every global, the CTask region, and the heap bump words out in BSS.
        let mut cursor = 0u64;
        let mut alloc_bss = |size: u64, align: u64| {
            let a = align.max(1);
            let off = cursor.div_ceil(a) * a;
            cursor = off + size.max(1);
            off
        };
        let global_bss: Vec<u64> = ir
            .globals
            .iter()
            .map(|g| alloc_bss(g.size as u64, g.align as u64))
            .collect();
        let ctask_bss = fs_gid.map(|_| alloc_bss(ctask_size, 16));
        if !heap_used.is_empty() {
            heap_globals = Some((alloc_bss(8, 8), alloc_bss(8, 8)));
        }
        bss_size = cursor;
        Ctx {
            freestanding: true,
            program_has_exceptions,
            global_sym: Vec::new(),
            global_bss,
            fs_gid,
            argc_gid,
            argv_gid,
            envp_gid,
            ctask_bss,
            fs_key_sym: None,
            ctask_size,
            exc_top_off,
            except_ch_off,
            heap_labels,
        }
    } else {
        // Darwin: globals are common symbols (`gid → ndefined + gid`). `Fs` is per-thread
        // via pthread TLS — when the program uses it, a hidden `pthread_key_t` common is
        // appended after the globals (no single shared CTask).
        let global_sym: Vec<u32> = (0..ir.globals.len() as u32)
            .map(|gid| ndefined + gid)
            .collect();
        let fs_key_sym = if prog_uses_fs {
            Some(ndefined + ir.globals.len() as u32)
        } else {
            None
        };
        Ctx {
            freestanding: false,
            program_has_exceptions,
            global_sym,
            global_bss: Vec::new(),
            fs_gid,
            argc_gid,
            argv_gid,
            envp_gid,
            ctask_bss: None,
            fs_key_sym,
            ctask_size,
            exc_top_off,
            except_ch_off,
            heap_labels: HashMap::new(),
        }
    };

    // Freestanding: emit `_start` first (the ELF entry). It materialises argc/argv/envp
    // from the initial stack into x0/x1/x2 (so `@entry`'s capture path is shared with
    // Darwin), calls `@entry`, then `exit_group`s with its return.
    if let Some(sl) = start_label {
        asm.place(sl);
        if ctx.argc_gid.is_some() || ctx.envp_gid.is_some() {
            asm.load_mem(0, SP, 8, false); // x0 = [sp] = argc
        }
        if ctx.argv_gid.is_some() {
            asm.add_imm(1, SP, 8); // x1 = &argv[0]
        }
        if ctx.envp_gid.is_some() {
            asm.load_imm(SCRATCH, 8);
            asm.mul(2, 0, SCRATCH); // x2 = argc * 8
            asm.add_imm(2, 2, 16); // + 16 (past argv[0..argc] and its NULL)
            asm.add_imm(SCRATCH, SP, 0); // x8 = sp (ADD #0 reads SP; MOV can't)
            asm.add(2, 2, SCRATCH); // x2 = sp + argc*8 + 16 = &envp[0]
        }
        asm.bl(labels[crate::lower::ENTRY]);
        asm.load_imm(SCRATCH, 94); // x8 = SYS_exit_group
        asm.svc(); // exit(x0)
    }

    for f in &reachable {
        let mut e = FnEmit {
            asm: &mut asm,
            labels: &labels,
            str_idx: &str_idx,
            ctx: &ctx,
            ret: f.ret,
            block_labels: Vec::new(),
            slot_off: Vec::new(),
            vreg_off: Vec::new(),
            vreg_reg: Vec::new(),
            saved_regs: Vec::new(),
            fs_cache_off: None,
            fusable: HashSet::new(),
            pending_fuse: HashMap::new(),
            mul_fusable: HashSet::new(),
            pending_mul: HashMap::new(),
            shift_fusable: HashSet::new(),
            pending_shift: HashMap::new(),
        };
        e.emit(f)?;
    }

    // Freestanding: emit the `mmap` heap runtime (the routines the program calls).
    if freestanding {
        if let Some((hp, he)) = heap_globals {
            emit_heap_runtime(&mut asm, &ctx.heap_labels, hp, he, uses_msize);
        }
    }

    let image = asm.finish()?;

    if freestanding {
        if !image.relocs.is_empty() {
            return Err(CodegenError::new(
                "freestanding aarch64-linux: this program uses a feature (libc call or \
                 unported primitive) not yet supported on the IR freestanding backend",
                None,
            ));
        }
        return Ok(target.write_executable(&image.text, bss_size));
    }

    // Hosted Darwin: a relocatable object with a symbol table.
    let mut defined: Vec<(String, u64)> = Vec::new();
    for f in &reachable {
        let sym = if f.name == crate::lower::ENTRY {
            "_main".to_string()
        } else {
            format!("_{}", f.name)
        };
        let off = image
            .label_bytes
            .get(labels[f.name.as_str()])
            .copied()
            .flatten()
            .ok_or_else(|| CodegenError::new("internal: unplaced IR function label", None))?;
        defined.push((sym, off));
    }
    let mut commons: Vec<(String, u64, u32)> = ir
        .globals
        .iter()
        .map(|g| {
            (
                format!("_{}", g.name),
                g.size.max(1) as u64,
                g.align.max(1).trailing_zeros(),
            )
        })
        .collect();
    // The hidden `pthread_key_t` for the per-thread `CTask` (appended last so its symbol
    // index matches `fs_key_sym`). Per-thread `Fs`: each thread lazily `malloc`s its own
    // `CTask` and `pthread_setspecific`s it under this key.
    if ctx.fs_key_sym.is_some() {
        commons.push(("__hcc_ir_fs_key".to_string(), 8, 3));
    }
    Ok(target.write_object(&image, &defined, &commons, ndefined))
}

/// Program-wide addressing / runtime context shared by every [`FnEmit`]. It selects
/// between the two AArch64 targets: hosted Darwin (a Mach-O object whose globals/CTask
/// are linker-resolved common symbols, with libc-call primitives) and freestanding
/// `aarch64-unknown-linux` (a self-contained static ELF whose globals/CTask live at
/// fixed BSS offsets, with raw-syscall primitives and an `mmap` heap).
struct Ctx {
    /// `true` for the freestanding static-ELF target (no libc, no linker).
    freestanding: bool,
    /// `true` if the program uses exceptions (`try`/`throw`) anywhere. Disables register
    /// promotion program-wide (an unwinding `throw` would not restore a promoted callee-saved
    /// register in an abandoned intermediate frame — see `backend::plan_registers`).
    program_has_exceptions: bool,
    /// Darwin: each global's symbol index (`gid → sym`). Empty when freestanding.
    global_sym: Vec<u32>,
    /// Freestanding: each global's BSS byte offset (`gid → off`). Empty when hosted.
    global_bss: Vec<u64>,
    /// `gid` of the implicit `Fs`/`argc`/`argv`/`envp` globals, when present.
    fs_gid: Option<u32>,
    argc_gid: Option<u32>,
    argv_gid: Option<u32>,
    envp_gid: Option<u32>,
    /// The hidden zeroed `CTask` region `Fs` points at on **freestanding** (a single-task
    /// BSS region; `@entry` stores its address into `Fs`). On Darwin there is no single
    /// region — `Fs` is **per-thread** via pthread TLS (see `fs_key_sym`).
    ctask_bss: Option<u64>,
    /// Darwin only: the common symbol of a hidden `pthread_key_t` for the per-thread
    /// `CTask`. `Some` iff the program actually uses `Fs`/exceptions. `@entry` creates the
    /// key; each `Fs`-using function caches this thread's `CTask*` in a frame slot
    /// (lazily `malloc`'d on first access), so the main thread and pthread-spawned threads
    /// get independent exception state.
    fs_key_sym: Option<u32>,
    /// `sizeof(CTask)` — the per-thread allocation size for the Darwin lazy create.
    ctask_size: u64,
    /// Byte offset of `CTask::exc_top` (the handler-frame chain head) and `CTask::except_ch`
    /// (the thrown value; read on an uncaught throw).
    exc_top_off: u32,
    except_ch_off: u32,
    /// Freestanding heap-runtime entry labels (`MAlloc`/`Free`/`HeapExtend`/`MSize`).
    /// Empty on Darwin (those map to libc).
    heap_labels: HashMap<&'static str, usize>,
}

/// Per-function emission state.
struct FnEmit<'a> {
    asm: &'a mut Asm,
    labels: &'a HashMap<&'a str, usize>,
    str_idx: &'a [usize],
    ctx: &'a Ctx,
    /// This function's return shape (selects an int vs float return register).
    ret: Ret,
    block_labels: Vec<usize>,
    /// Frame offset of each slot (address = `FP - off`).
    slot_off: Vec<u32>,
    /// Frame offset of each vreg's spill slot.
    vreg_off: Vec<u32>,
    /// Where each vreg lives: a physical register (`Location::Reg`) or its `vreg_off` slot
    /// (`Location::Spill`). From `backend::allocate`; all-`Spill` when the program uses
    /// exceptions (allocation disabled program-wide).
    vreg_reg: Vec<Location>,
    /// Callee-saved registers this function promotes into, with the frame offset where
    /// the caller's value is saved across the body: `(reg, is_float, off)`.
    saved_regs: Vec<(u32, bool, u32)>,
    /// Darwin: frame offset of this function's cached thread-local `CTask*` (filled in the
    /// prologue), when the function uses `Fs`. `&Fs` resolves to this slot's address.
    fs_cache_off: Option<u32>,
    /// Addressing-mode fusion (T3): the `PtrAdd` dst vregs that are used exactly once, by an
    /// *immediately-following* `Load`/`Store` in the same block, with a register-offset-legal
    /// stride. Such a `PtrAdd` is not emitted standalone; the indexing folds into the memory
    /// op. Adjacency is what makes this safe without confusing the (already-run) allocator's
    /// liveness — `base`/`index` are read at the fused op, exactly where the `PtrAdd` read them.
    fusable: HashSet<Vreg>,
    /// A deferred fusable `PtrAdd`'s `(base, index, stride)`, keyed by its dst, consumed by the
    /// next `Load`/`Store`.
    pending_fuse: HashMap<Vreg, (Val, Val, u32)>,
    /// Integer multiply-add fusion (T5a): `Mul` dst vregs used exactly once, by an immediately-
    /// following integer `Add`/`Sub` (the `Mul` as the subtrahend for `Sub`). Such a `Mul` is
    /// not emitted standalone; it folds into the `Add`/`Sub` as `madd`/`msub`. Same
    /// adjacency-keeps-liveness-valid argument as `fusable`.
    mul_fusable: HashSet<Vreg>,
    /// A deferred fusable `Mul`'s `(lhs, rhs)`, keyed by its dst, consumed by the next add/sub.
    pending_mul: HashMap<Vreg, (Val, Val)>,
    /// Shifted-register operand fusion: `Shl`/`Shr`-by-constant dst vregs used exactly once, by
    /// an immediately-following `Add`/`Sub`/`BitAnd`/`BitOr`/`BitXor` (the shift as the `rhs` for
    /// the non-commutative `Sub`). Such a shift is not emitted standalone; it folds into the
    /// consumer's shifted-register operand (`add x,a,b,lsl #k` etc.). Same
    /// adjacency-keeps-liveness-valid argument as `mul_fusable`.
    shift_fusable: HashSet<Vreg>,
    /// A deferred fusable shift's `(operand, shift-type, imm6)`, keyed by its dst, consumed by
    /// the next add/sub/logical op.
    pending_shift: HashMap<Vreg, (Val, u32, u32)>,
}

/// The `PtrAdd` dst vregs eligible for addressing-mode fusion (T3): used exactly once, by an
/// *immediately-following* `Load`/`Store` in the same block, with a register-offset-legal
/// stride (`1` or the access size — the unscaled / `LSL #log2(size)` forms). Restricting to
/// the adjacent single use keeps `base`/`index` evaluated at the same program point the
/// `PtrAdd` would have, so the already-computed register liveness stays valid.
fn compute_fusable(f: &Func) -> HashSet<Vreg> {
    use crate::backend::analysis::{term_uses, uses_of};
    let mut uses = vec![0u32; f.n_vregs as usize];
    for b in &f.blocks {
        for i in &b.insts {
            uses_of(i, |r| uses[r as usize] += 1);
        }
        term_uses(&b.term, |r| uses[r as usize] += 1);
    }
    let mut fusable = HashSet::new();
    for b in &f.blocks {
        for w in b.insts.windows(2) {
            let (dst, stride) = match &w[0] {
                Inst::PtrAdd { dst, stride, .. } => (*dst, *stride),
                _ => continue,
            };
            if uses[dst as usize] != 1 {
                continue;
            }
            let ok = match &w[1] {
                Inst::Load {
                    ty,
                    addr: Val::Reg(v),
                    ..
                }
                | Inst::Store {
                    ty,
                    addr: Val::Reg(v),
                    ..
                } if *v == dst => stride == 1 || stride == ty.size(),
                _ => false,
            };
            if ok {
                fusable.insert(dst);
            }
        }
    }
    fusable
}

/// The integer `Mul` dst vregs eligible for multiply-add fusion (T5a): used exactly once, by
/// an *immediately-following* integer `Add` (either operand) or `Sub` (as the subtrahend, the
/// `rhs`). `madd`/`msub` compute the low-64-bit `a±b*c` — identical to a wrapping `mul` then
/// `add`/`sub`, at any integer width. Adjacency keeps the deferred `Mul`'s operands live to
/// the fused op.
fn compute_mul_fusable(f: &Func) -> HashSet<Vreg> {
    use crate::backend::analysis::{term_uses, uses_of};
    let mut uses = vec![0u32; f.n_vregs as usize];
    for b in &f.blocks {
        for i in &b.insts {
            uses_of(i, |r| uses[r as usize] += 1);
        }
        term_uses(&b.term, |r| uses[r as usize] += 1);
    }
    let mut fusable = HashSet::new();
    for b in &f.blocks {
        for w in b.insts.windows(2) {
            let dst = match &w[0] {
                Inst::Bin {
                    dst,
                    op: BinOp::Mul,
                    ty,
                    ..
                } if !ty.is_float() => *dst,
                _ => continue,
            };
            if uses[dst as usize] != 1 {
                continue;
            }
            let ok = match &w[1] {
                Inst::Bin {
                    op: BinOp::Add,
                    ty,
                    lhs,
                    rhs,
                    ..
                } if !ty.is_float() => *lhs == Val::Reg(dst) || *rhs == Val::Reg(dst),
                Inst::Bin {
                    op: BinOp::Sub,
                    ty,
                    rhs,
                    ..
                } if !ty.is_float() => *rhs == Val::Reg(dst),
                _ => false,
            };
            if ok {
                fusable.insert(dst);
            }
        }
    }
    fusable
}

/// The constant-shift dst vregs eligible for shifted-register operand fusion: a `Shl`/`Shr` by
/// an immediate `0..=63`, used exactly once by an *immediately-following* integer
/// `Add`/`BitAnd`/`BitOr`/`BitXor` (either operand) or `Sub` (as the subtrahend, the `rhs`).
/// The shift folds into that op's shifted-register operand (`add x,a,b,lsl #k` etc.). The shift
/// is 64-bit either way (the standalone `Shr` is `lsrv`/`asrv` at 64-bit, narrowed by a separate
/// downstream `Cast`), so the fused single instruction is bit-identical. Adjacency keeps the
/// deferred shift's operand live to the fused op.
fn compute_shift_fusable(f: &Func) -> HashSet<Vreg> {
    use crate::backend::analysis::{term_uses, uses_of};
    let mut uses = vec![0u32; f.n_vregs as usize];
    for b in &f.blocks {
        for i in &b.insts {
            uses_of(i, |r| uses[r as usize] += 1);
        }
        term_uses(&b.term, |r| uses[r as usize] += 1);
    }
    let mut fusable = HashSet::new();
    for b in &f.blocks {
        for w in b.insts.windows(2) {
            let dst = match &w[0] {
                Inst::Bin {
                    dst,
                    op: BinOp::Shl | BinOp::Shr,
                    ty,
                    rhs: Val::ImmInt(k),
                    ..
                } if !ty.is_float() && (0..=63).contains(k) => *dst,
                _ => continue,
            };
            if uses[dst as usize] != 1 {
                continue;
            }
            let ok = match &w[1] {
                Inst::Bin {
                    op: BinOp::Add | BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor,
                    ty,
                    lhs,
                    rhs,
                    ..
                } if !ty.is_float() => *lhs == Val::Reg(dst) || *rhs == Val::Reg(dst),
                Inst::Bin {
                    op: BinOp::Sub,
                    ty,
                    rhs,
                    ..
                } if !ty.is_float() => *rhs == Val::Reg(dst),
                _ => false,
            };
            if ok {
                fusable.insert(dst);
            }
        }
    }
    fusable
}

/// If `c` is a power of two `2^k` with `k` in `1..=62`, return `k`. Mirrors `simplify::pow2`'s
/// bound (excludes `2^0`, an identity, and `2^63`, negative as `i64`), so the signed div/mod
/// strength reduction below covers exactly the cases `simplify` leaves for the backend.
fn pow2(c: i64) -> Option<u32> {
    if c > 1 && (c & (c - 1)) == 0 {
        let k = c.trailing_zeros();
        (1..=62).contains(&k).then_some(k)
    } else {
        None
    }
}

impl FnEmit<'_> {
    fn emit(&mut self, f: &Func) -> Result<(), CodegenError> {
        // Plan register promotion: hot vregs → callee-saved x19–x28 / d8–d15. A spilled
        // vreg (`None`) still gets a frame slot below; a promoted one lives in its
        // register, so its slot is dead but harmless.
        // Callee-saved pools (preserved across the body, saved/restored in the prologue) +
        // caller-saved pools (free — used only for ranges that don't cross a call). x13–x15
        // are caller-saved scratch the heap runtime uses, which is safe here because the heap
        // is reached via a `Prim` (a clobber point), so no caller-saved-promoted vreg is live
        // across it; v19–v31 are otherwise unused. x17 is likewise free: the isel's integer
        // scratch is x9–x12 + x16 (IND); x17 (IP1) is touched only by linker branch islands,
        // which veneer far `bl`s (clobber points) — never an in-function conditional branch —
        // so a caller-saved-promoted vreg in x17 is never live across an island. (x18 is
        // platform-reserved on Darwin.) The argument registers (x0–x7 / v0–v7) are excluded
        // until `place_args` does a parallel move (a later phase).
        let regset = crate::backend::RegSet {
            callee_int: vec![19, 20, 21, 22, 23, 24, 25, 26, 27, 28],
            caller_int: vec![13, 14, 15, 17],
            callee_flt: vec![8, 9, 10, 11, 12, 13, 14, 15],
            caller_flt: (19..=31).collect(),
        };
        let regs = crate::backend::allocate(f, &regset, self.ctx.program_has_exceptions);
        self.vreg_reg = regs.loc;
        self.fusable = compute_fusable(f);
        self.mul_fusable = compute_mul_fusable(f);
        self.shift_fusable = compute_shift_fusable(f);

        // ---- frame layout ----
        let mut frame = 0u32;
        let mut alloc = |size: u32, align: u32| {
            let a = align.max(1);
            frame = frame.div_ceil(a) * a + size.max(1);
            frame
        };
        self.slot_off = f.slots.iter().map(|s| alloc(s.size, s.align)).collect();
        self.vreg_off = (0..f.n_vregs).map(|_| alloc(8, 8)).collect();

        // One save slot per callee-saved register the allocator used: this function must
        // preserve x19–x28 / d8–d15 for its own caller, so it stashes the incoming value in
        // the prologue and restores it in the epilogue. (Caller-saved promotions, when Phase
        // 4 adds them, are not in `regs.saved` and need no slot.)
        self.saved_regs = regs
            .saved
            .iter()
            .map(|p| (p.num, p.is_float, alloc(8, 8)))
            .collect();
        // Darwin per-thread `Fs`: an `Fs`-using function reserves one slot to cache this
        // thread's `CTask*`, filled in the prologue; `&Fs` resolves to it.
        let uses_fs = !self.ctx.freestanding
            && self.ctx.fs_key_sym.is_some()
            && self
                .ctx
                .fs_gid
                .is_some_and(|g| crate::backend::func_uses_fs(f, g));
        if uses_fs {
            self.fs_cache_off = Some(alloc(8, 8));
        }
        let frame_size = (frame + 15) & !15; // 16-byte aligned

        self.block_labels = f.blocks.iter().map(|_| self.asm.new_label()).collect();

        // ---- prologue ----
        self.asm.place(self.labels[f.name.as_str()]);
        self.asm.stp_pre_fp_lr();
        self.asm.mov_fp_sp();
        // Reserve the frame. The spill-everything model can produce a frame larger
        // than a single `sub sp, sp, #imm12` (4095 bytes), so subtract in ≤4080-byte
        // (16-aligned) chunks. The epilogue restores `sp` from `x29`, so it needs no
        // matching adjustment.
        let mut rem = frame_size;
        while rem > 0 {
            let chunk = rem.min(4080);
            self.asm.sub_sp_imm(chunk);
            rem -= chunk;
        }

        // Save the caller's value of every callee-saved register we promote into. This
        // must precede the parameter stores below, which may overwrite a promoted
        // register with an incoming argument.
        for (reg, is_float, off) in self.saved_regs.clone() {
            self.fp_minus(ADDR, off);
            if is_float {
                self.asm.fstur(reg, ADDR, 0);
            } else {
                self.asm.store_mem(reg, ADDR, 8);
            }
        }

        // Store incoming parameters into their slots: int/ptr from x0.., F64 from
        // v0.., the two classes numbered independently (the internal ABI). An
        // aggregate-returning function takes its hidden leading `$sret` pointer in x8
        // (the sret register), not a general-purpose argument register.
        let mut params = f.params.iter();
        if matches!(f.ret, Ret::Agg { .. }) {
            if let Some(sret) = params.next() {
                self.store_vreg(sret.vreg, SCRATCH); // x8
            }
        }
        let mut igr = 0u32;
        let mut fpr = 0u32;
        for p in params {
            match p.ty {
                ArgTy::Float => {
                    self.store_float(p.vreg, fpr);
                    fpr += 1;
                }
                ArgTy::Int(_) | ArgTy::AggAddr { .. } => {
                    self.store_vreg(p.vreg, igr);
                    igr += 1;
                }
            }
        }

        if f.name == crate::lower::ENTRY {
            // Capture the command line before anything clobbers the arg registers:
            // `_main`/`_start` deliver argc/argv/envp in x0/x1/x2 (the freestanding
            // `_start` materialises them from the stack). `@entry` has no parameters, so
            // those registers are still live here.
            for (gid, reg) in [
                (self.ctx.argc_gid, 0u32),
                (self.ctx.argv_gid, 1),
                (self.ctx.envp_gid, 2),
            ] {
                if let Some(g) = gid {
                    self.global_addr_into(ADDR, g, 0);
                    self.asm.store_mem(reg, ADDR, 8);
                }
            }

            // Freestanding (single-task): seed `Fs` once to the hidden zeroed `CTask`
            // region. Darwin: create the per-thread pthread key (the per-thread `CTask`
            // is allocated lazily in each `Fs`-using function's prologue below).
            if self.ctx.freestanding {
                if self.ctx.fs_gid.is_some() && self.ctx.ctask_bss.is_some() {
                    self.ctask_addr_into(TMP0);
                    let fs = self.ctx.fs_gid.unwrap();
                    self.global_addr_into(ADDR, fs, 0);
                    self.asm.store_mem(TMP0, ADDR, 8);
                }
            } else if let Some(key) = self.ctx.fs_key_sym {
                // pthread_key_create(&key, NULL).
                self.asm.adrp_global(0, key);
                self.asm.add_global(0, 0, key);
                self.asm.load_imm(1, 0);
                self.asm.bl_extern("_pthread_key_create");
            }
        }

        // Darwin: cache this thread's `CTask*` into the frame slot (lazily creating it).
        if let Some(off) = self.fs_cache_off {
            self.emit_fs_cache(off);
        }

        // ---- body: walk the blocks via the shared driver ----
        crate::backend::emit_blocks(self, f)
    }

    fn unsupported(&self, what: &str) -> CodegenError {
        CodegenError::new(format!("IR arm64 backend: {what} not yet supported"), None)
    }

    // ---- global addressing (target-directed) ----

    /// Load `&global[gid] + off` into `reg`. Freestanding: a self-resolved `ADR` to the
    /// global's fixed BSS address. Darwin: the linker-relocated `ADRP`+`ADD` pair.
    fn global_addr_into(&mut self, reg: u32, gid: u32, off: u32) {
        // Darwin per-thread `Fs`: `&Fs` is the per-function frame slot caching this
        // thread's `CTask*` (so a `Load` of it yields the thread-local task, and
        // `Fs->field` is thread-local). `off` is always 0 for the `Fs` pointer.
        if !self.ctx.freestanding && off == 0 && Some(gid) == self.ctx.fs_gid {
            if let Some(cache) = self.fs_cache_off {
                self.fp_minus(reg, cache);
                return;
            }
        }
        if self.ctx.freestanding {
            let base = self.ctx.global_bss[gid as usize];
            self.asm.adr_global_fs(reg, base + off as u64);
        } else {
            let sym = self.ctx.global_sym[gid as usize];
            self.asm.adrp_global(reg, sym);
            self.asm.add_global(reg, reg, sym);
            if off != 0 {
                self.asm.add_imm(reg, reg, off);
            }
        }
    }

    /// Load the address of the hidden freestanding single-task `CTask` region into `reg`.
    fn ctask_addr_into(&mut self, reg: u32) {
        self.asm
            .adr_global_fs(reg, self.ctx.ctask_bss.expect("CTask BSS region"));
    }

    /// Darwin: fill the frame `CTask*` cache slot with this thread's task, computed via
    /// pthread TLS (`pthread_getspecific`; on first access per thread, `malloc` + zero a
    /// `CTask`, set its self-pointer, and `pthread_setspecific` it). Done once in the
    /// prologue, where clobbering the arg/scratch registers is safe.
    fn emit_fs_cache(&mut self, cache_off: u32) {
        let key = self.ctx.fs_key_sym.expect("pthread key");
        let size = self.ctx.ctask_size;
        let have = self.asm.new_label();
        let done = self.asm.new_label();
        // x0 = pthread_getspecific(*key)
        self.asm.adrp_global(0, key);
        self.asm.add_global(0, 0, key);
        self.asm.load_mem(0, 0, 8, false); // x0 = key value
        self.asm.bl_extern("_pthread_getspecific");
        self.asm.cbnz(0, have);
        // First access on this thread: x0 = malloc(sizeof CTask), zero it, set self.
        self.asm.load_imm(0, size as i64);
        self.asm.bl_extern("_malloc");
        let mut off = 0u32;
        while (off as u64) < size {
            self.asm.store_mem_off(31, 0, off, 8); // xzr -> [x0 + off]
            off += 8;
        }
        self.asm.store_mem_off(0, 0, 0, 8); // CTask.self = x0
        self.fp_minus(ADDR, cache_off);
        self.asm.store_mem(0, ADDR, 8); // cache = x0
        // pthread_setspecific(*key, ptr)
        self.asm.adrp_global(0, key);
        self.asm.add_global(0, 0, key);
        self.asm.load_mem(0, 0, 8, false); // x0 = key
        self.fp_minus(ADDR, cache_off);
        self.asm.load_mem(1, ADDR, 8, false); // x1 = ptr
        self.asm.bl_extern("_pthread_setspecific");
        self.asm.b(done);
        // Existing task on this thread: cache the pointer getspecific returned.
        self.asm.place(have);
        self.fp_minus(ADDR, cache_off);
        self.asm.store_mem(0, ADDR, 8);
        self.asm.place(done);
    }

    // ---- value access (spill-all) ----

    /// Set `reg = FP - off` (the address of a frame offset). Uses the 12-bit `sub`
    /// immediate when it fits, else materialises the offset (the spill-everything
    /// frame can exceed 4095 bytes).
    fn fp_minus(&mut self, reg: u32, off: u32) {
        if off <= 0xFFF {
            self.asm.sub_imm(reg, FP, off);
        } else {
            self.asm.load_imm(reg, off as i64);
            self.asm.sub(reg, FP, reg);
        }
    }

    /// Function epilogue: restore the caller's callee-saved registers (FP still valid, so
    /// addressing is `FP - off`), then tear down the frame.
    fn epilogue(&mut self) {
        for (reg, is_float, off) in self.saved_regs.clone() {
            self.fp_minus(ADDR, off);
            if is_float {
                self.asm.fldur(reg, ADDR, 0);
            } else {
                self.asm.load_mem(reg, ADDR, 8, false);
            }
        }
        self.asm.mov_sp_fp();
        self.asm.ldp_post_fp_lr();
        self.asm.ret();
    }

    /// Load integer/pointer operand `v`'s raw 64 bits into GPR `reg`. A float-promoted
    /// vreg lives in a d-register, so its bits are bridged out with `fmov` — this keeps
    /// the generic GPR movers (`Mov`/`Load`/`Store`/bit-copy `Cast`) correct for floats.
    fn load_val(&mut self, v: Val, reg: u32) {
        match v {
            Val::Reg(r) => match self.vreg_reg[r as usize] {
                Location::Reg(p) => {
                    if p.is_float {
                        self.asm.fmov_to_gpr(reg, p.num);
                    } else {
                        self.asm.mov_reg(reg, p.num);
                    }
                }
                Location::Spill => {
                    let off = self.vreg_off[r as usize];
                    self.fp_minus(reg, off);
                    self.asm.load_mem(reg, reg, 8, false);
                }
            },
            Val::ImmInt(i) => self.asm.load_imm(reg, i),
            Val::ImmF64(b) => self.asm.load_imm(reg, b as i64),
        }
    }

    /// `FRES = FRES % FT2` for F64 (IEEE remainder with a truncated quotient — `fmod`).
    /// Darwin calls libc `fmod` so the result is bit-identical to the interpreter's Rust
    /// `f64 % f64` (which is itself `fmod`). Freestanding has no libc, so it computes
    /// `a - trunc(a/b)*b` inline (exact for the usual small quotients).
    /// Float `%`: the truncated remainder `a - trunc(a/b)*b` (HolyC's `Fmod` form), on both
    /// targets. `frintz` truncates toward zero in the F64 domain, so it is magnitude-safe
    /// (an already-integral quotient is returned unchanged) and matches the interpreter's
    /// `f64::trunc` and the x86 backend byte-for-byte — including for huge quotients, where
    /// the old Darwin libc `_fmod` (an exact remainder) diverged from the freestanding path.
    fn emit_fmod(&mut self) {
        self.asm.fdiv(FT3, FRES, FT2); // a/b
        self.asm.frintz(FT3, FT3); // trunc(a/b)
        self.asm.fmul(FT3, FT3, FT2); // trunc(a/b)*b
        self.asm.fsub(FRES, FRES, FT3); // a - trunc(a/b)*b
    }

    /// Store GPR `reg`'s raw 64 bits into vreg `dst` (its promoted register, or its frame
    /// slot). A float-promoted `dst` lives in a d-register, so the bits are bridged in
    /// with `fmov` — keeping the generic GPR movers correct for floats.
    fn store_vreg(&mut self, dst: Vreg, reg: u32) {
        match self.vreg_reg[dst as usize] {
            Location::Reg(p) => {
                if p.is_float {
                    self.asm.fmov_from_gpr(p.num, reg);
                } else {
                    self.asm.mov_reg(p.num, reg);
                }
            }
            Location::Spill => {
                let off = self.vreg_off[dst as usize];
                self.fp_minus(ADDR, off);
                self.asm.store_mem(reg, ADDR, 8);
            }
        }
    }

    fn slot_addr(&mut self, slot: SlotId, off: u32, reg: u32) {
        let base = self.slot_off[slot as usize];
        self.fp_minus(reg, base);
        if off != 0 {
            self.asm.add_imm(reg, reg, off);
        }
    }

    /// Load a float operand `v` into FP register `vr` (from its promoted d-register or
    /// its slot's 64 bits).
    fn load_float(&mut self, v: Val, vr: u32) {
        match v {
            Val::Reg(r) => match self.vreg_reg[r as usize] {
                Location::Reg(p) => self.asm.fmov_reg(vr, p.num),
                Location::Spill => {
                    let off = self.vreg_off[r as usize];
                    self.fp_minus(ADDR, off);
                    self.asm.fldur(vr, ADDR, 0);
                }
            },
            Val::ImmF64(b) => {
                self.asm.load_imm(TMP0, b as i64);
                self.asm.fmov_from_gpr(vr, TMP0);
            }
            Val::ImmInt(i) => {
                self.asm.load_imm(TMP0, i);
                self.asm.fmov_from_gpr(vr, TMP0);
            }
        }
    }

    /// Store FP register `vr`'s 64 bits into vreg `dst` (its promoted d-register or slot).
    fn store_float(&mut self, dst: Vreg, vr: u32) {
        match self.vreg_reg[dst as usize] {
            Location::Reg(p) => self.asm.fmov_reg(p.num, vr),
            Location::Spill => {
                let off = self.vreg_off[dst as usize];
                self.fp_minus(ADDR, off);
                self.asm.fstur(vr, ADDR, 0);
            }
        }
    }

    // ---- instruction selection ----

    fn sel_inst(&mut self, inst: &Inst) -> Result<(), CodegenError> {
        match inst {
            Inst::Bin {
                dst,
                op,
                ty,
                signed,
                lhs,
                rhs,
            } => {
                if ty.is_float() {
                    self.load_float(*lhs, FRES);
                    self.load_float(*rhs, FT2);
                    match op {
                        BinOp::Add => self.asm.fadd(FRES, FRES, FT2),
                        BinOp::Sub => self.asm.fsub(FRES, FRES, FT2),
                        BinOp::Mul => self.asm.fmul(FRES, FRES, FT2),
                        BinOp::Div => self.asm.fdiv(FRES, FRES, FT2),
                        BinOp::Mod => self.emit_fmod(),
                        _ => return Err(self.unsupported("bitwise op on a float")),
                    }
                    self.store_float(*dst, FRES);
                } else if !self.try_shift_fuse(*dst, *op, *lhs, *rhs, *signed)
                    && !self.try_mul_fuse(*dst, *op, *lhs, *rhs)
                    && !self.try_imm_div(*dst, *op, *signed, *lhs, *rhs)
                {
                    self.load_val(*lhs, TMP0);
                    // Immediate-form strength reduction: `TMP0 op= #imm` directly when the
                    // constant fits, instead of materializing it in a scratch register first.
                    let imm_done = match *rhs {
                        Val::ImmInt(i) => self.try_imm_binop(*op, *signed, i),
                        _ => false,
                    };
                    if !imm_done {
                        self.load_val(*rhs, TMP1);
                        self.emit_int_binop(*op, *signed);
                    }
                    self.store_vreg(*dst, TMP0);
                }
            }
            Inst::Un { dst, op, ty, src } => {
                if ty.is_float() {
                    match op {
                        UnOp::Neg => {
                            self.load_float(*src, FRES);
                            self.asm.fneg(FRES, FRES);
                            self.store_float(*dst, FRES);
                        }
                        UnOp::BitNot => return Err(self.unsupported("bitwise not on a float")),
                        UnOp::Popcount => return Err(self.unsupported("popcount on a float")),
                    }
                } else if matches!(op, UnOp::Popcount) {
                    // 64-bit popcount: x → v16; cnt v16.8b (per-byte counts); addv b16 (sum the
                    // eight bytes); v16 → x.
                    self.load_val(*src, TMP0);
                    self.asm.fmov_from_gpr(FRES, TMP0);
                    self.asm.cnt_8b(FRES, FRES);
                    self.asm.addv_8b(FRES, FRES);
                    self.asm.fmov_to_gpr(TMP0, FRES);
                    self.store_vreg(*dst, TMP0);
                } else {
                    self.load_val(*src, TMP0);
                    match op {
                        UnOp::Neg => self.asm.neg(TMP0, TMP0),
                        UnOp::BitNot => self.asm.mvn(TMP0, TMP0),
                        UnOp::Popcount => unreachable!("handled above"),
                    }
                    self.store_vreg(*dst, TMP0);
                }
            }
            Inst::Cmp {
                dst,
                op,
                ty,
                signed,
                lhs,
                rhs,
            } => {
                if ty.is_float() {
                    self.load_float(*lhs, FRES);
                    self.load_float(*rhs, FT2);
                    self.asm.fcmp(FRES, FT2);
                    self.asm.cset(TMP0, float_cond(*op));
                } else {
                    self.load_val(*lhs, TMP0);
                    // Compare against a small constant directly (`cmp TMP0, #imm`) instead of
                    // materializing it; the flags are identical to the register form, so the
                    // signed/unsigned `cset` condition is unchanged.
                    let imm_done = match *rhs {
                        Val::ImmInt(i) if (0..=4095).contains(&i) => {
                            self.asm.cmp_imm(TMP0, i as u32);
                            true
                        }
                        _ => false,
                    };
                    if !imm_done {
                        self.load_val(*rhs, TMP1);
                        self.asm.cmp_reg(TMP0, TMP1);
                    }
                    self.asm.cset(TMP0, cmp_cond(*op, *signed));
                }
                self.store_vreg(*dst, TMP0);
            }
            Inst::Cast { dst, to, from, src } => {
                match (from.is_float(), to.is_float()) {
                    (false, false) => {
                        self.load_val(*src, TMP0);
                        self.emit_int_cast(*to);
                        self.store_vreg(*dst, TMP0);
                    }
                    (true, true) => {
                        // F64 → F64: a bit copy. Routed through an FP register so a
                        // promoted float vreg (in d8–d15) is moved with `fmov`, not `mov`.
                        self.load_float(*src, FRES);
                        self.store_float(*dst, FRES);
                    }
                    (false, true) => {
                        // int → F64 (signed, matching the interpreter's `i as f64`).
                        self.load_val(*src, TMP0);
                        self.asm.scvtf(FRES, TMP0);
                        self.store_float(*dst, FRES);
                    }
                    (true, false) => {
                        // F64 → int: unsigned destination via fcvtzu, else fcvtzs; then
                        // narrow to the destination width.
                        self.load_float(*src, FRES);
                        if matches!(to, Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64) {
                            self.asm.fcvtzu(TMP0, FRES);
                        } else {
                            self.asm.fcvtzs(TMP0, FRES);
                        }
                        self.emit_int_cast(*to);
                        self.store_vreg(*dst, TMP0);
                    }
                }
            }
            // A move is a 64-bit copy. It must travel through the right register class:
            // a float vreg may be promoted to d8–d15, where only `fmov` (not `mov`)
            // applies. Spilled vregs still round-trip the raw bits either way.
            Inst::Mov { dst, src, ty } => {
                if ty.is_float() {
                    self.load_float(*src, FRES);
                    self.store_float(*dst, FRES);
                } else {
                    self.load_val(*src, TMP0);
                    self.store_vreg(*dst, TMP0);
                }
            }
            Inst::SlotAddr { dst, slot, off } => {
                self.slot_addr(*slot, *off, TMP0);
                self.store_vreg(*dst, TMP0);
            }
            Inst::StrAddr { dst, str } => {
                let sidx = self.str_idx[*str as usize];
                self.asm.adr(TMP0, sidx);
                self.store_vreg(*dst, TMP0);
            }
            Inst::FuncAddr { dst, func } => {
                let label = *self
                    .labels
                    .get(func.as_str())
                    .ok_or_else(|| self.unsupported("address of unlowered function"))?;
                self.asm.adr_label(TMP0, label);
                self.store_vreg(*dst, TMP0);
            }
            Inst::PtrAdd {
                dst,
                base,
                index,
                stride,
            } => {
                // Fusable into the next mem op: defer (emit nothing); the `Load`/`Store` folds
                // the indexing into a register-offset addressing mode.
                if self.fusable.contains(dst) {
                    self.pending_fuse.insert(*dst, (*base, *index, *stride));
                } else {
                    self.load_val(*base, TMP0);
                    self.load_val(*index, TMP1);
                    if *stride != 1 {
                        self.asm.load_imm(TMP2, *stride as i64);
                        self.asm.mul(TMP1, TMP1, TMP2);
                    }
                    self.asm.add(TMP0, TMP0, TMP1);
                    self.store_vreg(*dst, TMP0);
                }
            }
            // Loads/stores move raw bits through a GPR; an F64 is just an 8-byte
            // transfer, so no FP register is needed here.
            Inst::Load { dst, ty, addr } => {
                if let Some((base, index, stride)) =
                    addr.reg().and_then(|v| self.pending_fuse.remove(&v))
                {
                    // base/index read here (where the deferred PtrAdd read them) into scratch,
                    // before the result reg is written — so a promoted base/index aliasing dst
                    // is consumed first.
                    self.load_val(base, TMP0);
                    self.load_val(index, TMP1);
                    let scaled = stride == ty.size();
                    self.asm
                        .load_reg(TMP0, TMP0, TMP1, ty.size(), ty.is_signed(), scaled);
                    self.store_vreg(*dst, TMP0);
                } else {
                    self.load_val(*addr, TMP0);
                    self.asm.load_mem(TMP0, TMP0, ty.size(), ty.is_signed());
                    self.store_vreg(*dst, TMP0);
                }
            }
            Inst::Store { ty, addr, val } => {
                if let Some((base, index, stride)) =
                    addr.reg().and_then(|v| self.pending_fuse.remove(&v))
                {
                    self.load_val(base, TMP0);
                    self.load_val(index, TMP1);
                    self.load_val(*val, TMP2);
                    let scaled = stride == ty.size();
                    self.asm.store_reg(TMP2, TMP0, TMP1, ty.size(), scaled);
                } else {
                    self.load_val(*addr, ADDR);
                    self.load_val(*val, TMP0);
                    self.asm.store_mem(TMP0, ADDR, ty.size());
                }
            }
            Inst::MemZero { dst, len } => {
                self.load_val(*dst, TMP0);
                self.asm.load_imm(TMP1, 0);
                self.copy_fill(*len, false);
            }
            Inst::MemCpy { dst, src, len } => {
                self.load_val(*dst, TMP0);
                self.load_val(*src, ADDR);
                self.copy_fill(*len, true);
            }
            Inst::Call {
                dst,
                ret,
                callee,
                args,
                sret,
                ..
            } => self.emit_call(*dst, *ret, callee, args, *sret)?,
            Inst::Prim {
                dst,
                prim,
                args,
                width,
            } => self.emit_prim(*dst, *prim, args, *width)?,
            Inst::TryBegin { pad, frame } => self.emit_try_begin(*pad, *frame),
            Inst::TryEnd => self.emit_try_end(),
            Inst::GlobalAddr { dst, global, off } => {
                self.global_addr_into(TMP0, *global, *off);
                self.store_vreg(*dst, TMP0);
            }
        }
        Ok(())
    }

    /// Unrolled `memcpy`/`memzero`. For copy, `TMP0`=dst, `ADDR`=src. For zero,
    /// `TMP0`=dst, `TMP1`=0. Uses 8-byte chunks then a 1-byte tail. The base pointers are
    /// *advanced* per chunk rather than addressed via a running offset, so an aggregate
    /// larger than the scaled-immediate reach (≈32 KiB for an 8-byte access) — e.g. a big
    /// local array zero-initialised by `MemZero` — copies correctly instead of wrapping
    /// the offset and corrupting memory.
    fn copy_fill(&mut self, len: u32, copy: bool) {
        let mut off = 0u32;
        while off + 8 <= len {
            if copy {
                self.asm.load_mem(TMP1, ADDR, 8, false);
                self.asm.add_imm(ADDR, ADDR, 8);
            }
            self.asm.store_mem(TMP1, TMP0, 8);
            self.asm.add_imm(TMP0, TMP0, 8);
            off += 8;
        }
        while off < len {
            if copy {
                self.asm.load_mem(TMP1, ADDR, 1, false);
                self.asm.add_imm(ADDR, ADDR, 1);
            }
            self.asm.store_mem(TMP1, TMP0, 1);
            self.asm.add_imm(TMP0, TMP0, 1);
            off += 1;
        }
    }

    /// Integer multiply-add fusion (T5a). Either defer a fusable `Mul` (record its operands,
    /// emit nothing) or, at an `Add`/`Sub` consuming a deferred `Mul`, emit a single
    /// `madd`/`msub`. Returns true when it handled `dst = lhs <op> rhs`; false leaves it to the
    /// ordinary integer path. `base`/`index`-style safety: the deferred `Mul`'s operands are
    /// read here (at the adjacent consumer), where the allocator still has them live.
    fn try_mul_fuse(&mut self, dst: Vreg, op: BinOp, lhs: Val, rhs: Val) -> bool {
        match op {
            BinOp::Mul if self.mul_fusable.contains(&dst) => {
                self.pending_mul.insert(dst, (lhs, rhs));
                true
            }
            BinOp::Add => {
                if let Some((a, b)) = lhs.reg().and_then(|v| self.pending_mul.remove(&v)) {
                    self.emit_muladd(dst, a, b, rhs, false);
                    true
                } else if let Some((a, b)) = rhs.reg().and_then(|v| self.pending_mul.remove(&v)) {
                    self.emit_muladd(dst, a, b, lhs, false);
                    true
                } else {
                    false
                }
            }
            BinOp::Sub => {
                // Only `lhs - (a*b)` maps to `msub` (= lhs - a*b); a Mul on the lhs does not.
                if let Some((a, b)) = rhs.reg().and_then(|v| self.pending_mul.remove(&v)) {
                    self.emit_muladd(dst, a, b, lhs, true);
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    /// Emit `dst = acc ± a*b` as a single `madd`/`msub` (`sub` selects `msub`). Operands are
    /// read into scratch before the result register is written.
    fn emit_muladd(&mut self, dst: Vreg, a: Val, b: Val, acc: Val, sub: bool) {
        self.load_val(a, TMP0);
        self.load_val(b, TMP1);
        self.load_val(acc, TMP2);
        if sub {
            self.asm.msub(TMP0, TMP0, TMP1, TMP2);
        } else {
            self.asm.madd(TMP0, TMP0, TMP1, TMP2);
        }
        self.store_vreg(dst, TMP0);
    }

    /// Shifted-register operand fusion (mirrors [`Self::try_mul_fuse`]). Either defer a fusable
    /// `Shl`/`Shr`-by-constant (record its operand + shift kind/amount, emit nothing), or, at an
    /// `Add`/`Sub`/`BitAnd`/`BitOr`/`BitXor` consuming a deferred shift, emit a single shifted ALU
    /// op (`add x,a,b,lsl #k` etc.). Returns true when it handled `dst = lhs <op> rhs`.
    ///
    /// Conflict safety: a producer is in at most one of `shift_fusable`/`mul_fusable` (disjoint
    /// ops), and `compute_*_fusable`'s `windows(2)` adjacency means only the instruction at `i-1`
    /// is ever deferred — two distinct producers cannot both be immediately-adjacent to the same
    /// consumer — so at most one pending map has an entry keyed by a given consumer's operands.
    /// Trying shift-fuse before mul-fuse (and short-circuiting on success) therefore can't strand
    /// the other's deferred producer.
    fn try_shift_fuse(&mut self, dst: Vreg, op: BinOp, lhs: Val, rhs: Val, signed: bool) -> bool {
        match op {
            BinOp::Shl | BinOp::Shr if self.shift_fusable.contains(&dst) => {
                let Val::ImmInt(k) = rhs else { return false };
                let shtype = match op {
                    BinOp::Shl => SH_LSL,
                    _ if signed => SH_ASR,
                    _ => SH_LSR,
                };
                self.pending_shift
                    .insert(dst, (lhs, shtype, k as u32 & 0x3F));
                true
            }
            BinOp::Add | BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor => {
                // Commutative: the shifted operand may be either side.
                if let Some((s, sh, k)) = rhs.reg().and_then(|v| self.pending_shift.remove(&v)) {
                    self.emit_shifted(dst, op, lhs, s, sh, k);
                    true
                } else if let Some((s, sh, k)) =
                    lhs.reg().and_then(|v| self.pending_shift.remove(&v))
                {
                    self.emit_shifted(dst, op, rhs, s, sh, k);
                    true
                } else {
                    false
                }
            }
            BinOp::Sub => {
                // Only `a - (b<<k)`: the shifted operand must be Rm (the rhs).
                if let Some((s, sh, k)) = rhs.reg().and_then(|v| self.pending_shift.remove(&v)) {
                    self.emit_shifted(dst, BinOp::Sub, lhs, s, sh, k);
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    /// Emit `dst = base <op> (shifted <shtype> #imm6)` as one shifted-register ALU instruction.
    /// `base`/`shifted` are read into scratch before the result register is written.
    fn emit_shifted(&mut self, dst: Vreg, op: BinOp, base: Val, shifted: Val, sh: u32, imm6: u32) {
        self.load_val(base, TMP0);
        self.load_val(shifted, TMP1);
        match op {
            BinOp::Add => self.asm.add_shifted(TMP0, TMP0, TMP1, sh, imm6),
            BinOp::Sub => self.asm.sub_shifted(TMP0, TMP0, TMP1, sh, imm6),
            BinOp::BitAnd => self.asm.and_shifted(TMP0, TMP0, TMP1, sh, imm6),
            BinOp::BitOr => self.asm.orr_shifted(TMP0, TMP0, TMP1, sh, imm6),
            BinOp::BitXor => self.asm.eor_shifted(TMP0, TMP0, TMP1, sh, imm6),
            _ => unreachable!("emit_shifted: non-fusable consumer op {op:?}"),
        }
        self.store_vreg(dst, TMP0);
    }

    /// Signed divide/modulo by a power of two `2^k`, strength-reduced to a spill-free
    /// round-toward-zero sequence instead of a `sdiv` (~12-cycle). Returns false (the caller
    /// falls back to the generic `sdiv` path) unless `op` is signed `Div`/`Mod` and `rhs` is a
    /// constant power of two. (Unsigned div/mod by `2^k` is already reduced to a shift/mask in
    /// `simplify`, so it never reaches here as `Div`/`Mod`.)
    ///
    /// `bias = (2^k - 1) if x < 0 else 0` is `(x >>s 63) >>u (64 - k)`; then
    /// `q = (x + bias) >>s k` truncates toward zero (bias-free for non-negative `x`, and the
    /// `+2^k-1` rounds a negative quotient up to truncation), bit-identical to `sdiv`. `Mod`
    /// forms `r = x - (q << k)` (the existing `sdiv; msub` remainder), reusing `sub_shifted`.
    fn try_imm_div(&mut self, dst: Vreg, op: BinOp, signed: bool, lhs: Val, rhs: Val) -> bool {
        if !signed || !matches!(op, BinOp::Div | BinOp::Mod) {
            return false;
        }
        let Val::ImmInt(c) = rhs else { return false };
        let Some(k) = pow2(c) else { return false };
        self.load_val(lhs, TMP0); // x
        self.asm.asr_imm(TMP1, TMP0, 63); // sign mask: all-ones if x<0 else 0
        self.asm.lsr_imm(TMP1, TMP1, 64 - k); // bias = (2^k - 1) if x<0 else 0
        if matches!(op, BinOp::Div) {
            self.asm.add(TMP0, TMP0, TMP1); // x + bias
            self.asm.asr_imm(TMP0, TMP0, k); // q = (x + bias) >>s k
        } else {
            self.asm.add(TMP1, TMP0, TMP1); // x + bias
            self.asm.asr_imm(TMP1, TMP1, k); // q
            self.asm.sub_shifted(TMP0, TMP0, TMP1, SH_LSL, k); // r = x - (q << k)
        }
        self.store_vreg(dst, TMP0);
        true
    }

    /// Emit `TMP0 = TMP0 <op> #imm` with an immediate-form instruction when `imm` is
    /// encodable, avoiding the materialize-into-scratch the register form needs. Returns
    /// false (the caller then loads `imm` into a register and uses `emit_int_binop`) when
    /// there is no usable immediate form. The 64-bit register width matches `emit_int_binop`.
    fn try_imm_binop(&mut self, op: BinOp, signed: bool, imm: i64) -> bool {
        const M: i64 = 4095; // add/sub take a 12-bit unsigned immediate
        match op {
            BinOp::Add if (0..=M).contains(&imm) => self.asm.add_imm(TMP0, TMP0, imm as u32),
            BinOp::Add if (-M..0).contains(&imm) => self.asm.sub_imm(TMP0, TMP0, -imm as u32),
            BinOp::Sub if (0..=M).contains(&imm) => self.asm.sub_imm(TMP0, TMP0, imm as u32),
            BinOp::Sub if (-M..0).contains(&imm) => self.asm.add_imm(TMP0, TMP0, -imm as u32),
            BinOp::Shl if (0..=63).contains(&imm) => self.asm.lsl_imm(TMP0, TMP0, imm as u32),
            BinOp::Shr if !signed && (0..=63).contains(&imm) => {
                self.asm.lsr_imm(TMP0, TMP0, imm as u32)
            }
            BinOp::Shr if signed && (0..=63).contains(&imm) => {
                self.asm.asr_imm(TMP0, TMP0, imm as u32)
            }
            // AND/OR/XOR with a constant that is a legal bitmask immediate (a rotated run of
            // set bits, e.g. `& 0x7FFFFFFF`); otherwise fall back to the register form.
            BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor => {
                match crate::backend::arm64::asm::encode_logical_imm(imm as u64) {
                    Some((n, immr, imms)) => match op {
                        BinOp::BitAnd => self.asm.and_imm(TMP0, TMP0, n, immr, imms),
                        BinOp::BitOr => self.asm.orr_imm(TMP0, TMP0, n, immr, imms),
                        _ => self.asm.eor_imm(TMP0, TMP0, n, immr, imms),
                    },
                    None => return false,
                }
            }
            _ => return false,
        }
        true
    }

    fn emit_int_binop(&mut self, op: BinOp, signed: bool) {
        match op {
            BinOp::Add => self.asm.add(TMP0, TMP0, TMP1),
            BinOp::Sub => self.asm.sub(TMP0, TMP0, TMP1),
            BinOp::Mul => self.asm.mul(TMP0, TMP0, TMP1),
            BinOp::Div => {
                if signed {
                    self.asm.sdiv(TMP0, TMP0, TMP1)
                } else {
                    self.asm.udiv(TMP0, TMP0, TMP1)
                }
            }
            BinOp::Mod => {
                if signed {
                    self.asm.sdiv(ADDR, TMP0, TMP1)
                } else {
                    self.asm.udiv(ADDR, TMP0, TMP1)
                }
                self.asm.msub(TMP0, ADDR, TMP1, TMP0); // TMP0 - (q * TMP1)
            }
            BinOp::BitAnd => self.asm.and(TMP0, TMP0, TMP1),
            BinOp::BitOr => self.asm.orr(TMP0, TMP0, TMP1),
            BinOp::BitXor => self.asm.eor(TMP0, TMP0, TMP1),
            BinOp::Shl => self.asm.lslv(TMP0, TMP0, TMP1),
            BinOp::Shr => {
                if signed {
                    self.asm.asrv(TMP0, TMP0, TMP1)
                } else {
                    self.asm.lsrv(TMP0, TMP0, TMP1)
                }
            }
        }
    }

    /// Narrow/extend `TMP0` to `to`'s width.
    fn emit_int_cast(&mut self, to: Ty) {
        match to {
            Ty::I8 => self.asm.sbfm(TMP0, TMP0, 0, 7),
            Ty::U8 => self.asm.ubfm(TMP0, TMP0, 0, 7),
            Ty::I16 => self.asm.sbfm(TMP0, TMP0, 0, 15),
            Ty::U16 => self.asm.ubfm(TMP0, TMP0, 0, 15),
            Ty::I32 => self.asm.sbfm(TMP0, TMP0, 0, 31),
            Ty::U32 => self.asm.ubfm(TMP0, TMP0, 0, 31),
            Ty::I64 | Ty::U64 | Ty::Ptr => {}
            Ty::F64 => {}
        }
    }

    fn emit_call(
        &mut self,
        dst: Option<Vreg>,
        ret: Ret,
        callee: &Callee,
        args: &[ArgVal],
        sret: Option<Val>,
    ) -> Result<(), CodegenError> {
        // An optimization intrinsic (`Sqrt`/`Fabs`/the rounding family) with the
        // `F64 -> F64` shape lowers to a single FP instruction instead of calling its
        // lib body — the body is correctly rounded, so the two agree bit-for-bit and
        // the interpreter (which always runs the body) stays in conformance.
        if let Callee::Direct(name) = callee {
            if self.try_intrinsic(name, ret, args, dst) {
                return Ok(());
            }
        }
        self.place_args(args, sret)?;
        match callee {
            Callee::Direct(name) => {
                let label = *self
                    .labels
                    .get(name.as_str())
                    .ok_or_else(|| self.unsupported("call to unlowered function"))?;
                self.asm.bl(label);
            }
            Callee::Indirect(v) => {
                self.load_val(*v, IND);
                self.asm.blr(IND);
            }
        }
        self.deliver_result(dst, ret);
        Ok(())
    }

    /// Place call arguments in the ABI registers (int/ptr in x0.., aggregate address
    /// also in an x register; sret pointer in x8). Floats are not yet handled.
    fn place_args(&mut self, args: &[ArgVal], sret: Option<Val>) -> Result<(), CodegenError> {
        if let Some(s) = sret {
            self.load_val(s, SCRATCH); // x8 = sret pointer
        }
        let mut igr = 0u32;
        let mut fpr = 0u32;
        for a in args {
            match a.ty {
                ArgTy::Float => {
                    if fpr > 7 {
                        return Err(self.unsupported("more than 8 float arguments"));
                    }
                    self.load_float(a.val, fpr);
                    fpr += 1;
                }
                ArgTy::Int(_) | ArgTy::AggAddr { .. } => {
                    if igr > 7 {
                        return Err(self.unsupported("more than 8 integer arguments"));
                    }
                    self.load_val(a.val, igr);
                    igr += 1;
                }
            }
        }
        Ok(())
    }

    fn deliver_result(&mut self, dst: Option<Vreg>, ret: Ret) {
        if let Some(d) = dst {
            match ret {
                Ret::Scalar(t) if t.is_float() => self.store_float(d, 0), // d0
                _ => self.store_vreg(d, 0),                               // x0
            }
        }
    }

    /// Lower a recognized algebraic/rounding optimization intrinsic to a single FP
    /// instruction in place of a call to its lib body. Fires only for the exact
    /// `F64 -> F64` single-argument shape (one float arg, float result, a result
    /// register), so a user override with a different signature falls through to an
    /// ordinary call. Returns whether it was handled.
    fn try_intrinsic(&mut self, name: &str, ret: Ret, args: &[ArgVal], dst: Option<Vreg>) -> bool {
        if crate::intrinsics::kind(name) != Some(crate::intrinsics::IntrinsicKind::Optimization) {
            return false;
        }
        let (Some(d), [arg]) = (dst, args) else {
            return false;
        };
        if !matches!(ret, Ret::Scalar(Ty::F64)) || !matches!(arg.ty, ArgTy::Float) {
            return false;
        }
        let emit: fn(&mut Asm, u32, u32) = match name {
            "Sqrt" => Asm::fsqrt,
            "Fabs" => Asm::fabs,
            "Floor" => Asm::frintm,
            "Ceil" => Asm::frintp,
            "Trunc" => Asm::frintz,
            "Round" => Asm::frinta,
            "RoundToEven" => Asm::frintn,
            _ => return false,
        };
        self.load_float(arg.val, FRES);
        emit(&mut self.asm, FRES, FRES);
        self.store_float(d, FRES);
        true
    }

    fn sel_term(&mut self, term: &Term) -> Result<(), CodegenError> {
        match term {
            Term::Br(t) => self.asm.b(self.block_labels[*t as usize]),
            Term::CondBr { cond, t, f } => {
                let (tl, fl) = (
                    self.block_labels[*t as usize],
                    self.block_labels[*f as usize],
                );
                match cond {
                    Cond::NonZero { val, ty } => {
                        if ty.is_float() {
                            // Truthy ⇔ != 0.0 (and NaN is truthy, matching the oracle).
                            self.load_float(*val, FRES);
                            self.asm.fcmp_zero(FRES);
                            self.asm.b_cond(C_NE, tl);
                        } else {
                            self.load_val(*val, TMP0);
                            self.asm.cbnz(TMP0, tl);
                        }
                    }
                    Cond::Cmp {
                        op,
                        ty,
                        signed,
                        lhs,
                        rhs,
                    } => {
                        if ty.is_float() {
                            self.load_float(*lhs, FRES);
                            self.load_float(*rhs, FT2);
                            self.asm.fcmp(FRES, FT2);
                            self.asm.b_cond(float_cond(*op), tl);
                        } else {
                            self.load_val(*lhs, TMP0);
                            self.load_val(*rhs, TMP1);
                            self.asm.cmp_reg(TMP0, TMP1);
                            self.asm.b_cond(cmp_cond(*op, *signed), tl);
                        }
                    }
                }
                self.asm.b(fl);
            }
            Term::Switch {
                val,
                cases,
                default,
                ..
            } => {
                let default_label = self.block_labels[*default as usize];
                if self.try_switch_table(*val, cases, default_label) {
                    return Ok(());
                }
                // Compare-chain fallback (sparse or wide-range switches).
                self.load_val(*val, TMP0);
                for (lo, hi, blk) in cases {
                    let target = self.block_labels[*blk as usize];
                    if lo == hi {
                        self.asm.load_imm(TMP1, *lo);
                        self.asm.cmp_reg(TMP0, TMP1);
                        self.asm.b_cond(C_EQ, target);
                    } else {
                        let skip = self.asm.new_label();
                        self.asm.load_imm(TMP1, *lo);
                        self.asm.cmp_reg(TMP0, TMP1);
                        self.asm.b_cond(C_LT, skip);
                        self.asm.load_imm(TMP1, *hi);
                        self.asm.cmp_reg(TMP0, TMP1);
                        self.asm.b_cond(C_LE, target);
                        self.asm.place(skip);
                    }
                }
                self.asm.b(default_label);
            }
            Term::Ret(v) => {
                match v {
                    // A float result returns in d0; everything else in x0.
                    Some(val) if matches!(self.ret, Ret::Scalar(t) if t.is_float()) => {
                        self.load_float(*val, 0)
                    }
                    Some(val) => self.load_val(*val, 0),
                    None => self.asm.load_imm(0, 0), // void: exit code 0 for `_main`
                }
                self.epilogue();
            }
            // The throw value and the `Fs` flags (`except_ch`, `catch_except`) were
            // written by the `Store`s the lowering emits before this terminator, so both
            // `throw expr;` and a bare `throw;` (re-raise) reduce to the same unwind.
            Term::Throw(_) | Term::Rethrow => self.emit_unwind(),
            Term::Unreachable => self.epilogue(),
        }
        Ok(())
    }

    /// Dispatch a dense switch through an O(1) jump table instead of the compare-chain;
    /// returns `true` when it emitted the table. The table is `span` 32-bit offset words
    /// (`table[k] = label_k - table`); dispatch is `idx = v - min`, an unsigned bounds
    /// check, then `LDRSW off, [table, idx, lsl #2]; BR table + off`. Out-of-range and
    /// gap values branch to `default`; overlapping ranges resolve to the first covering
    /// case — both matching the compare-chain. Fires only when there are ≥4 cases and the
    /// covered value span is small and dense enough to be worth a table.
    fn try_switch_table(
        &mut self,
        val: Val,
        cases: &[(i64, i64, BlockId)],
        default: usize,
    ) -> bool {
        if cases.len() < 4 || cases.iter().any(|(lo, hi, _)| hi < lo) {
            return false;
        }
        let min = cases.iter().map(|c| c.0).min().unwrap();
        let max = cases.iter().map(|c| c.1).max().unwrap();
        let span = (max - min + 1) as usize;
        if span > 1024 || span > cases.len().saturating_mul(4).max(8) {
            return false;
        }

        // Map each value to its first covering case; gaps fall to `default`.
        let mut slots = vec![default; span];
        let mut filled = vec![false; span];
        for (lo, hi, blk) in cases {
            for v in *lo..=*hi {
                let k = (v - min) as usize;
                if !filled[k] {
                    filled[k] = true;
                    slots[k] = self.block_labels[*blk as usize];
                }
            }
        }

        self.load_val(val, TMP0); // TMP0 = v
        if min != 0 {
            self.asm.load_imm(TMP1, min);
            self.asm.sub(TMP0, TMP0, TMP1); // TMP0 = v - min
        }
        self.asm.load_imm(TMP1, (span - 1) as i64);
        self.asm.cmp_reg(TMP0, TMP1);
        self.asm.b_cond(C_HI, default); // unsigned out-of-range -> default
        let table = self.asm.new_label();
        self.asm.adr_label(TMP1, table); // TMP1 = &table
        self.asm.ldrsw_reg(TMP2, TMP1, TMP0); // TMP2 = table[idx] (signed byte offset)
        self.asm.add(TMP1, TMP1, TMP2); // TMP1 = &table + offset = target
        self.asm.br(TMP1); // the table data below is never executed as code
        self.asm.place(table);
        for slot in slots {
            self.asm.table_word(table, slot);
        }
        true
    }
}

/// The block-walk driver ([`crate::backend::emit_blocks`]) drives a `FnEmit` through these
/// per-arch leaf emits.
impl crate::backend::Backend for FnEmit<'_> {
    fn place_block(&mut self, i: usize) {
        self.asm.place(self.block_labels[i]);
    }
    fn emit_inst(&mut self, inst: &Inst) -> Result<(), CodegenError> {
        self.sel_inst(inst)
    }
    fn emit_term(&mut self, term: &Term) -> Result<(), CodegenError> {
        self.sel_term(term)
    }
}

/// The condition code that holds when `lhs <op> rhs` for floats, chosen so an
/// unordered (NaN) compare is false for `< <= > >=` and `==`, true for `!=` —
/// matching the interpreter's IEEE comparisons.
fn float_cond(op: CmpOp) -> u32 {
    match op {
        CmpOp::Eq => C_EQ,
        CmpOp::Ne => C_NE,
        CmpOp::Lt => C_MI,
        CmpOp::Le => C_LS,
        CmpOp::Gt => C_GT,
        CmpOp::Ge => C_GE,
    }
}

/// The condition code that holds when `lhs <op> rhs` (per signedness).
fn cmp_cond(op: CmpOp, signed: bool) -> u32 {
    match op {
        CmpOp::Eq => C_EQ,
        CmpOp::Ne => C_NE,
        CmpOp::Lt => {
            if signed {
                C_LT
            } else {
                C_LO
            }
        }
        CmpOp::Le => {
            if signed {
                C_LE
            } else {
                C_LS
            }
        }
        CmpOp::Gt => {
            if signed {
                C_GT
            } else {
                C_HI
            }
        }
        CmpOp::Ge => {
            if signed {
                C_GE
            } else {
                C_HS
            }
        }
    }
}

/// Native end-to-end tests for the IR backend: compile via `compile_ir`, link with
/// `cc`, run, and compare to the IR interpreter (the oracle). Apple-silicon macOS only (the
/// one host that can both emit and execute these), self-skipping elsewhere.
#[cfg(all(test, target_arch = "aarch64", target_os = "macos"))]
#[path = "tests/isel.rs"]
mod tests;
