//! IR-driven AArch64 code generation — the arm64 backend.
//!
//! This lowers a program to the SSA [IR](crate::ir), destructs it out of SSA
//! ([`crate::regalloc::destruct_ssa`]), and emits AArch64 by walking the resulting
//! `phi`-free blocks — reusing the [`Asm`](super::asm::Asm) encoder, the Mach-O object
//! writer, and the freestanding-ELF writer. It is the sole arm64 backend for both
//! `Arm64Darwin` and `Arm64Linux` (the old AST-walking codegen is deleted); the full
//! `tests/arm64_darwin.rs` conformance suite passes through it.
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
//! linear scan ([`crate::regalloc::plan_registers`]) promotes hot vregs into the
//! callee-saved registers (x19–x28 / d8–d15), saved/restored in the prologue/epilogue;
//! a `try`-containing function stays fully spilled. Handles the deterministic language:
//! integers/pointers/floats, memory, control flow (incl. an O(1) jump-table `switch`),
//! direct and indirect calls, by-value aggregates (sret), globals, string literals,
//! exceptions (`try`/`catch`/`throw` via a jmp_buf/longjmp unwind over `Fs->exc_top`),
//! the command line (`ArgC`/`ArgV`/`EnvP`), and the pure-HolyC printf path. The impure
//! primitives are all lowered: the heap, the clock, fd/file I/O, sockets, fs mutation,
//! process ids, atomics/futex, and threads — `pthread` on Darwin, raw `clone(2)` + futex
//! join freestanding. The algebraic intrinsics `Sqrt`/`Fabs`/the rounding family lower
//! to single FP instructions in place of their lib bodies. Every example compiles and
//! matches the oracle on Darwin (verified by execution); freestanding ELFs are executed
//! on a linux/aarch64 host on CI.

use std::collections::HashMap;

use super::asm::Asm;
use super::{ArmTarget, FP, SCRATCH, SP};
use crate::codegen::CodegenError;
use crate::ir::*;

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

/// Compile `program` through the IR pipeline to either a hosted Darwin Mach-O object
/// or a freestanding `aarch64-unknown-linux` static ELF, selected by `target`.
pub(super) fn compile_ir(
    program: &crate::ast::Program,
    target: &dyn ArmTarget,
) -> Result<Vec<u8>, CodegenError> {
    let (layouts, _) = crate::layout::compute(program);
    let ir = crate::lower::lower(program, &layouts)?;
    let ir = crate::regalloc::destruct_program(&ir);

    let by_name: HashMap<&str, &IrFunc> = ir.funcs.iter().map(|f| (f.name.as_str(), f)).collect();

    // Reachable functions from `@entry` (the top-level driver), over direct calls and
    // address-taken functions. We emit only these, so the symbol table is complete.
    let mut reachable: Vec<&IrFunc> = Vec::new();
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut queue: Vec<&str> = Vec::new();
    if by_name.contains_key(crate::lower::ENTRY) {
        queue.push(crate::lower::ENTRY);
    }
    while let Some(name) = queue.pop() {
        if !seen.insert(name) {
            continue;
        }
        let Some(f) = by_name.get(name) else {
            return Err(CodegenError::new(
                format!("IR arm64 backend: needed function `{name}` was not lowered"),
                None,
            ));
        };
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

    // Implicit-global ids, shared by both targets.
    let gid_of = |name: &str| {
        ir.globals
            .iter()
            .position(|g| g.name == name)
            .map(|i| i as u32)
    };
    let fs_gid = gid_of("Fs");
    let argc_gid = gid_of("ArgC");
    let argv_gid = gid_of("ArgV");
    let envp_gid = gid_of("EnvP");
    let (exc_top_off, except_ch_off) = if fs_gid.is_some() {
        let field = |name: &str| {
            ir.layouts.offset_of("CTask", name).ok_or_else(|| {
                CodegenError::new(format!("IR arm64 backend: CTask has no {name}"), None)
            })
        };
        (field("exc_top")? as u32, field("except_ch")? as u32)
    } else {
        (0, 0)
    };
    let ctask_size = ir
        .layouts
        .size_of(&crate::ast::Type::Named("CTask".to_string()))
        .max(8) as u64;
    // Whether the reachable program actually touches `Fs`/exceptions — drives the Darwin
    // per-thread pthread-TLS setup (a non-exception program needs none of it).
    let prog_uses_fs = fs_gid.is_some_and(|g| reachable.iter().any(|f| func_uses_fs(f, g)));

    // Which heap primitives the reachable code uses (the freestanding `mmap` runtime
    // emits exactly these; `MSize` makes `MAlloc`/`HeapExtend` carry a size header).
    let mut heap_used: std::collections::HashSet<&'static str> = std::collections::HashSet::new();
    for f in &reachable {
        for b in &f.blocks {
            for inst in &b.insts {
                if let IrInst::Prim { prim, .. } = inst {
                    match prim {
                        Prim::MAlloc => heap_used.insert("MAlloc"),
                        Prim::Free => heap_used.insert("Free"),
                        Prim::HeapExtend => heap_used.insert("HeapExtend"),
                        Prim::MSize => heap_used.insert("MSize"),
                        _ => false,
                    };
                }
            }
        }
    }
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
        commons.push(("__solomon_ir_fs_key".to_string(), 8, 3));
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
    /// Darwin: each global's symbol index (`gid → sym`). Empty when freestanding.
    global_sym: Vec<u32>,
    /// Freestanding: each global's BSS byte offset (`gid → off`). Empty when hosted.
    global_bss: Vec<u64>,
    /// `gid` of the implicit `Fs`/`ArgC`/`ArgV`/`EnvP` globals, when present.
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
    ret: IrRet,
    block_labels: Vec<usize>,
    /// Frame offset of each slot (address = `FP - off`).
    slot_off: Vec<u32>,
    /// Frame offset of each vreg's spill slot.
    vreg_off: Vec<u32>,
    /// Register-promotion plan: `vreg → Some(callee-saved reg)` when promoted to
    /// x19–x28 / d8–d15, else `None` (lives in its `vreg_off` slot). Empty for a
    /// function with a `try` (promotion is disabled there — see `plan_registers`).
    vreg_reg: Vec<Option<crate::regalloc::PReg>>,
    /// Callee-saved registers this function promotes into, with the frame offset where
    /// the caller's value is saved across the body: `(reg, is_float, off)`.
    saved_regs: Vec<(u32, bool, u32)>,
    /// Darwin: frame offset of this function's cached thread-local `CTask*` (filled in the
    /// prologue), when the function uses `Fs`. `&Fs` resolves to this slot's address.
    fs_cache_off: Option<u32>,
}

impl FnEmit<'_> {
    fn emit(&mut self, f: &IrFunc) -> Result<(), CodegenError> {
        // Plan register promotion: hot vregs → callee-saved x19–x28 / d8–d15. A spilled
        // vreg (`None`) still gets a frame slot below; a promoted one lives in its
        // register, so its slot is dead but harmless.
        self.vreg_reg = crate::regalloc::plan_registers(f);

        // ---- frame layout ----
        let mut frame = 0u32;
        let mut alloc = |size: u32, align: u32| {
            let a = align.max(1);
            frame = frame.div_ceil(a) * a + size.max(1);
            frame
        };
        self.slot_off = f.slots.iter().map(|s| alloc(s.size, s.align)).collect();
        self.vreg_off = (0..f.n_vregs).map(|_| alloc(8, 8)).collect();

        // One save slot per distinct callee-saved register we promote into: this
        // function must preserve x19–x28 / d8–d15 for its own caller, so it stashes the
        // incoming value in the prologue and restores it in the epilogue.
        let mut used: Vec<(u32, bool)> = Vec::new();
        for p in self.vreg_reg.iter().flatten() {
            if !used.iter().any(|&(r, fl)| r == p.num && fl == p.is_float) {
                used.push((p.num, p.is_float));
            }
        }
        self.saved_regs = used
            .into_iter()
            .map(|(reg, is_float)| (reg, is_float, alloc(8, 8)))
            .collect();
        // Darwin per-thread `Fs`: an `Fs`-using function reserves one slot to cache this
        // thread's `CTask*`, filled in the prologue; `&Fs` resolves to it.
        let uses_fs = !self.ctx.freestanding
            && self.ctx.fs_key_sym.is_some()
            && self.ctx.fs_gid.is_some_and(|g| func_uses_fs(f, g));
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
        if matches!(f.ret, IrRet::Agg { .. }) {
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

        // ---- body ----
        for (i, b) in f.blocks.iter().enumerate() {
            self.asm.place(self.block_labels[i]);
            for inst in &b.insts {
                self.emit_inst(inst, f)?;
            }
            self.emit_term(&b.term, f)?;
        }
        Ok(())
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
    /// prologue, where clobbering the arg/scratch registers is safe. Mirrors the AST
    /// backend's `emit_fs_cache`.
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

    /// Load integer/pointer operand `v`'s raw 64 bits into GPR `reg`. A float-promoted
    /// vreg lives in a d-register, so its bits are bridged out with `fmov` — this keeps
    /// the generic GPR movers (`Mov`/`Load`/`Store`/bit-copy `Cast`) correct for floats.
    fn load_val(&mut self, v: Val, reg: u32) {
        match v {
            Val::Reg(r) => {
                if let Some(p) = self.vreg_reg[r as usize] {
                    if p.is_float {
                        self.asm.fmov_to_gpr(reg, p.num);
                    } else {
                        self.asm.mov_reg(reg, p.num);
                    }
                } else {
                    let off = self.vreg_off[r as usize];
                    self.fp_minus(reg, off);
                    self.asm.load_mem(reg, reg, 8, false);
                }
            }
            Val::ImmInt(i) => self.asm.load_imm(reg, i),
            Val::ImmF64(b) => self.asm.load_imm(reg, b as i64),
        }
    }

    /// `FRES = FRES % FT2` for F64 (IEEE remainder with a truncated quotient — `fmod`).
    /// Darwin calls libc `fmod` so the result is bit-identical to the interpreter's Rust
    /// `f64 % f64` (which is itself `fmod`). Freestanding has no libc, so it computes
    /// `a - trunc(a/b)*b` inline (exact for the usual small quotients).
    fn emit_fmod(&mut self) {
        if self.ctx.freestanding {
            self.asm.fdiv(FT3, FRES, FT2); // a/b
            self.asm.frintz(FT3, FT3); // trunc(a/b)
            self.asm.fmul(FT3, FT3, FT2); // trunc(a/b)*b
            self.asm.fsub(FRES, FRES, FT3); // a - trunc(a/b)*b
        } else {
            self.asm.fmov_reg(0, FRES); // v0 = a
            self.asm.fmov_reg(1, FT2); // v1 = b
            self.asm.bl_extern("_fmod");
            self.asm.fmov_reg(FRES, 0); // FRES = fmod(a, b)
        }
    }

    /// Store GPR `reg`'s raw 64 bits into vreg `dst` (its promoted register, or its frame
    /// slot). A float-promoted `dst` lives in a d-register, so the bits are bridged in
    /// with `fmov` — keeping the generic GPR movers correct for floats.
    fn store_vreg(&mut self, dst: Vreg, reg: u32) {
        if let Some(p) = self.vreg_reg[dst as usize] {
            if p.is_float {
                self.asm.fmov_from_gpr(p.num, reg);
            } else {
                self.asm.mov_reg(p.num, reg);
            }
        } else {
            let off = self.vreg_off[dst as usize];
            self.fp_minus(ADDR, off);
            self.asm.store_mem(reg, ADDR, 8);
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
            Val::Reg(r) => {
                if let Some(p) = self.vreg_reg[r as usize] {
                    self.asm.fmov_reg(vr, p.num);
                } else {
                    let off = self.vreg_off[r as usize];
                    self.fp_minus(ADDR, off);
                    self.asm.fldur(vr, ADDR, 0);
                }
            }
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
        if let Some(p) = self.vreg_reg[dst as usize] {
            self.asm.fmov_reg(p.num, vr);
        } else {
            let off = self.vreg_off[dst as usize];
            self.fp_minus(ADDR, off);
            self.asm.fstur(vr, ADDR, 0);
        }
    }

    // ---- instruction selection ----

    fn emit_inst(&mut self, inst: &IrInst, _f: &IrFunc) -> Result<(), CodegenError> {
        match inst {
            IrInst::Bin {
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
                        IrBinOp::Add => self.asm.fadd(FRES, FRES, FT2),
                        IrBinOp::Sub => self.asm.fsub(FRES, FRES, FT2),
                        IrBinOp::Mul => self.asm.fmul(FRES, FRES, FT2),
                        IrBinOp::Div => self.asm.fdiv(FRES, FRES, FT2),
                        IrBinOp::Mod => self.emit_fmod(),
                        _ => return Err(self.unsupported("bitwise op on a float")),
                    }
                    self.store_float(*dst, FRES);
                } else {
                    self.load_val(*lhs, TMP0);
                    self.load_val(*rhs, TMP1);
                    self.emit_int_binop(*op, *signed);
                    self.store_vreg(*dst, TMP0);
                }
            }
            IrInst::Un { dst, op, ty, src } => {
                if ty.is_float() {
                    match op {
                        IrUnOp::Neg => {
                            self.load_float(*src, FRES);
                            self.asm.fneg(FRES, FRES);
                            self.store_float(*dst, FRES);
                        }
                        IrUnOp::BitNot => return Err(self.unsupported("bitwise not on a float")),
                    }
                } else {
                    self.load_val(*src, TMP0);
                    match op {
                        IrUnOp::Neg => self.asm.neg(TMP0, TMP0),
                        IrUnOp::BitNot => self.asm.mvn(TMP0, TMP0),
                    }
                    self.store_vreg(*dst, TMP0);
                }
            }
            IrInst::Cmp {
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
                    self.load_val(*rhs, TMP1);
                    self.asm.cmp_reg(TMP0, TMP1);
                    self.asm.cset(TMP0, cmp_cond(*op, *signed));
                }
                self.store_vreg(*dst, TMP0);
            }
            IrInst::Cast { dst, to, from, src } => {
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
                        if matches!(to, IrTy::U8 | IrTy::U16 | IrTy::U32 | IrTy::U64) {
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
            IrInst::Mov { dst, src, ty } => {
                if ty.is_float() {
                    self.load_float(*src, FRES);
                    self.store_float(*dst, FRES);
                } else {
                    self.load_val(*src, TMP0);
                    self.store_vreg(*dst, TMP0);
                }
            }
            IrInst::SlotAddr { dst, slot, off } => {
                self.slot_addr(*slot, *off, TMP0);
                self.store_vreg(*dst, TMP0);
            }
            IrInst::StrAddr { dst, str } => {
                let sidx = self.str_idx[*str as usize];
                self.asm.adr(TMP0, sidx);
                self.store_vreg(*dst, TMP0);
            }
            IrInst::FuncAddr { dst, func } => {
                let label = *self
                    .labels
                    .get(func.as_str())
                    .ok_or_else(|| self.unsupported("address of unlowered function"))?;
                self.asm.adr_label(TMP0, label);
                self.store_vreg(*dst, TMP0);
            }
            IrInst::PtrAdd {
                dst,
                base,
                index,
                stride,
            } => {
                self.load_val(*base, TMP0);
                self.load_val(*index, TMP1);
                if *stride != 1 {
                    self.asm.load_imm(TMP2, *stride as i64);
                    self.asm.mul(TMP1, TMP1, TMP2);
                }
                self.asm.add(TMP0, TMP0, TMP1);
                self.store_vreg(*dst, TMP0);
            }
            // Loads/stores move raw bits through a GPR; an F64 is just an 8-byte
            // transfer, so no FP register is needed here.
            IrInst::Load { dst, ty, addr } => {
                self.load_val(*addr, TMP0);
                self.asm.load_mem(TMP0, TMP0, ty.size(), ty.is_signed());
                self.store_vreg(*dst, TMP0);
            }
            IrInst::Store { ty, addr, val } => {
                self.load_val(*addr, ADDR);
                self.load_val(*val, TMP0);
                self.asm.store_mem(TMP0, ADDR, ty.size());
            }
            IrInst::MemZero { dst, len } => {
                self.load_val(*dst, TMP0);
                self.asm.load_imm(TMP1, 0);
                self.copy_fill(*len, false);
            }
            IrInst::MemCpy { dst, src, len } => {
                self.load_val(*dst, TMP0);
                self.load_val(*src, ADDR);
                self.copy_fill(*len, true);
            }
            IrInst::Call {
                dst,
                ret,
                callee,
                args,
                sret,
                ..
            } => self.emit_call(*dst, *ret, callee, args, *sret)?,
            IrInst::Prim {
                dst,
                prim,
                args,
                width,
            } => self.emit_prim(*dst, *prim, args, *width)?,
            IrInst::TryBegin { pad, frame } => self.emit_try_begin(*pad, *frame),
            IrInst::TryEnd => self.emit_try_end(),
            IrInst::GlobalAddr { dst, global, off } => {
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

    fn emit_int_binop(&mut self, op: IrBinOp, signed: bool) {
        match op {
            IrBinOp::Add => self.asm.add(TMP0, TMP0, TMP1),
            IrBinOp::Sub => self.asm.sub(TMP0, TMP0, TMP1),
            IrBinOp::Mul => self.asm.mul(TMP0, TMP0, TMP1),
            IrBinOp::Div => {
                if signed {
                    self.asm.sdiv(TMP0, TMP0, TMP1)
                } else {
                    self.asm.udiv(TMP0, TMP0, TMP1)
                }
            }
            IrBinOp::Mod => {
                if signed {
                    self.asm.sdiv(ADDR, TMP0, TMP1)
                } else {
                    self.asm.udiv(ADDR, TMP0, TMP1)
                }
                self.asm.msub(TMP0, ADDR, TMP1, TMP0); // TMP0 - (q * TMP1)
            }
            IrBinOp::BitAnd => self.asm.and(TMP0, TMP0, TMP1),
            IrBinOp::BitOr => self.asm.orr(TMP0, TMP0, TMP1),
            IrBinOp::BitXor => self.asm.eor(TMP0, TMP0, TMP1),
            IrBinOp::Shl => self.asm.lslv(TMP0, TMP0, TMP1),
            IrBinOp::Shr => {
                if signed {
                    self.asm.asrv(TMP0, TMP0, TMP1)
                } else {
                    self.asm.lsrv(TMP0, TMP0, TMP1)
                }
            }
        }
    }

    /// Narrow/extend `TMP0` to `to`'s width.
    fn emit_int_cast(&mut self, to: IrTy) {
        match to {
            IrTy::I8 => self.asm.sbfm(TMP0, TMP0, 0, 7),
            IrTy::U8 => self.asm.ubfm(TMP0, TMP0, 0, 7),
            IrTy::I16 => self.asm.sbfm(TMP0, TMP0, 0, 15),
            IrTy::U16 => self.asm.ubfm(TMP0, TMP0, 0, 15),
            IrTy::I32 => self.asm.sbfm(TMP0, TMP0, 0, 31),
            IrTy::U32 => self.asm.ubfm(TMP0, TMP0, 0, 31),
            IrTy::I64 | IrTy::U64 | IrTy::Ptr => {}
            IrTy::F64 => {}
        }
    }

    fn emit_call(
        &mut self,
        dst: Option<Vreg>,
        ret: IrRet,
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

    fn deliver_result(&mut self, dst: Option<Vreg>, ret: IrRet) {
        if let Some(d) = dst {
            match ret {
                IrRet::Scalar(t) if t.is_float() => self.store_float(d, 0), // d0
                _ => self.store_vreg(d, 0),                                 // x0
            }
        }
    }

    /// Lower a recognized algebraic/rounding optimization intrinsic to a single FP
    /// instruction in place of a call to its lib body. Fires only for the exact
    /// `F64 -> F64` single-argument shape (one float arg, float result, a result
    /// register), so a user override with a different signature falls through to an
    /// ordinary call. Returns whether it was handled.
    fn try_intrinsic(
        &mut self,
        name: &str,
        ret: IrRet,
        args: &[ArgVal],
        dst: Option<Vreg>,
    ) -> bool {
        if crate::intrinsics::kind(name) != Some(crate::intrinsics::IntrinsicKind::Optimization) {
            return false;
        }
        let (Some(d), [arg]) = (dst, args) else {
            return false;
        };
        if !matches!(ret, IrRet::Scalar(IrTy::F64)) || !matches!(arg.ty, ArgTy::Float) {
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

    fn emit_prim(
        &mut self,
        dst: Option<Vreg>,
        prim: Prim,
        args: &[Val],
        width: Option<IrTy>,
    ) -> Result<(), CodegenError> {
        // `Open` needs the per-OS flag/ABI handling.
        if let Prim::Open = prim {
            return self.emit_open(dst, args);
        }
        // Atomics (width-directed), the memory fence, and the futex.
        if matches!(
            prim,
            Prim::AtomicLoad
                | Prim::AtomicStore
                | Prim::AtomicAdd
                | Prim::AtomicSwap
                | Prim::AtomicCas
        ) {
            return self.emit_atomic(dst, prim, args, width.unwrap_or(IrTy::I64));
        }
        if let Prim::AtomicFence = prim {
            self.asm.dmb_ish();
            return Ok(());
        }
        if matches!(prim, Prim::FutexWait | Prim::FutexWake) {
            return self.emit_futex(prim, args);
        }
        if matches!(prim, Prim::Thread | Prim::Join) {
            return self.emit_thread(dst, prim, args);
        }
        // The clock primitives read a per-clock-id timespec; `Sleep` builds one.
        if matches!(prim, Prim::UnixNS | Prim::NanoNS | Prim::CpuNS) {
            self.emit_clock(dst, prim);
            return Ok(());
        }
        if let Prim::Sleep = prim {
            self.emit_sleep(args)?;
            return Ok(());
        }
        // The heap primitives: freestanding calls the `mmap` runtime routines; Darwin
        // maps them to libc (`HeapExtend` has no libc equivalent, so it returns NULL and
        // `ReAlloc` falls back to allocate-copy-free).
        if matches!(
            prim,
            Prim::MAlloc | Prim::Free | Prim::HeapExtend | Prim::MSize
        ) {
            return self.emit_heap_prim(dst, prim, args);
        }
        // Identity/process and filesystem-mutation ops branch on the target internally.
        match prim {
            Prim::Getpid | Prim::Getppid | Prim::Getuid | Prim::Getgid => {
                return self.emit_procid(dst, prim);
            }
            Prim::Remove | Prim::Rename | Prim::Mkdir | Prim::Chdir => {
                return self.emit_fsop(dst, prim, args);
            }
            Prim::Getcwd => return self.emit_getcwd(dst, args),
            _ => {}
        }
        if self.ctx.freestanding {
            return self.emit_syscall_prim(dst, prim, args);
        }
        // Hosted Darwin: the remaining supported primitives map to libc calls. The ones
        // returning a C `int` (`close`/`socket`/`connect`) are sign-extended to I64.
        let (sym, sext) = match prim {
            Prim::StdWrite | Prim::Write => ("_write", false),
            Prim::Read => ("_read", false),
            Prim::LSeek => ("_lseek", false),
            Prim::Close => ("_close", true),
            Prim::Socket => ("_socket", true),
            Prim::Connect => ("_connect", true),
            Prim::Exit => ("_exit", false),
            other => {
                return Err(self.unsupported(&format!("primitive {other:?}")));
            }
        };
        self.place_prim_args(args)?;
        self.asm.bl_extern(sym);
        if let Some(d) = dst {
            if sext {
                self.asm.mov_reg(TMP0, 0);
                self.emit_int_cast(IrTy::I32);
                self.store_vreg(d, TMP0);
            } else {
                self.store_vreg(d, 0);
            }
        }
        Ok(())
    }

    /// `Getpid`/`Getppid`/`Getuid`/`Getgid` → the id in x0. Freestanding: a bare syscall;
    /// Darwin: libc, with the `int`/`uint` result extended to I64 (every real id is a
    /// small non-negative value, so the extend signedness is immaterial).
    fn emit_procid(&mut self, dst: Option<Vreg>, prim: Prim) -> Result<(), CodegenError> {
        if self.ctx.freestanding {
            let nr: i64 = match prim {
                Prim::Getpid => 172,
                Prim::Getppid => 173,
                Prim::Getuid => 174,
                _ => 176, // Getgid
            };
            self.asm.load_imm(SCRATCH, nr);
            self.asm.svc();
        } else {
            let sym = match prim {
                Prim::Getpid => "_getpid",
                Prim::Getppid => "_getppid",
                Prim::Getuid => "_getuid",
                _ => "_getgid",
            };
            self.asm.bl_extern(sym);
        }
        if let Some(d) = dst {
            self.asm.mov_reg(TMP0, 0);
            let to = if matches!(prim, Prim::Getuid | Prim::Getgid) {
                IrTy::U32
            } else {
                IrTy::I32
            };
            self.emit_int_cast(to);
            self.store_vreg(d, TMP0);
        }
        Ok(())
    }

    /// `Remove`/`Rename`/`Mkdir`/`Chdir` → 0 or `-errno`. Freestanding uses the `*at`
    /// syscalls (no bare `unlink`/`rename`/`mkdir`) with an `AT_FDCWD` prepend; `chdir`
    /// is bare. Darwin calls libc and converts the `-1`/errno failure to `-errno`.
    fn emit_fsop(
        &mut self,
        dst: Option<Vreg>,
        prim: Prim,
        args: &[Val],
    ) -> Result<(), CodegenError> {
        if self.ctx.freestanding {
            match prim {
                Prim::Remove => {
                    self.load_val(args[0], 1); // x1 = path
                    self.asm.load_imm(0, -100); // x0 = AT_FDCWD
                    self.asm.load_imm(2, 0); // x2 = flags
                    self.asm.load_imm(SCRATCH, 35); // SYS_unlinkat
                }
                Prim::Chdir => {
                    self.load_val(args[0], 0); // x0 = path
                    self.asm.load_imm(SCRATCH, 49); // SYS_chdir
                }
                Prim::Rename => {
                    self.load_val(args[0], 1); // x1 = oldpath
                    self.load_val(args[1], 3); // x3 = newpath
                    self.asm.load_imm(0, -100); // x0 = AT_FDCWD (old)
                    self.asm.load_imm(2, -100); // x2 = AT_FDCWD (new)
                    self.asm.load_imm(SCRATCH, 38); // SYS_renameat
                }
                Prim::Mkdir => {
                    self.load_val(args[0], 1); // x1 = path
                    self.load_val(args[1], 2); // x2 = mode
                    self.asm.load_imm(0, -100); // x0 = AT_FDCWD
                    self.asm.load_imm(SCRATCH, 34); // SYS_mkdirat
                }
                _ => unreachable!(),
            }
            self.asm.svc();
            if let Some(d) = dst {
                self.store_vreg(d, 0); // 0 / -errno
            }
            return Ok(());
        }
        let sym = match prim {
            Prim::Remove => "_unlink",
            Prim::Rename => "_rename",
            Prim::Mkdir => "_mkdir",
            Prim::Chdir => "_chdir",
            _ => unreachable!(),
        };
        self.place_prim_args(args)?; // path[, newpath/mode] in x0[, x1]
        self.asm.bl_extern(sym);
        self.asm.mov_reg(TMP0, 0);
        self.emit_int_cast(IrTy::I32); // sign-extend the libc `int`
        self.emit_errno_neg(); // -1 → -errno (normalised)
        if let Some(d) = dst {
            self.store_vreg(d, TMP0);
        }
        Ok(())
    }

    /// `Getcwd(buf, size)` → 0 or `-errno`. Freestanding `getcwd` returns the byte length
    /// on success (normalised to 0); Darwin libc returns `buf` (non-NULL → 0) or NULL
    /// (→ `-errno`).
    fn emit_getcwd(&mut self, dst: Option<Vreg>, args: &[Val]) -> Result<(), CodegenError> {
        self.place_prim_args(args)?; // x0 = buf, x1 = size
        if self.ctx.freestanding {
            self.asm.load_imm(SCRATCH, 17); // SYS_getcwd
            self.asm.svc();
            self.asm.mov_reg(TMP0, 0);
            // A non-negative length becomes 0; a negative -errno passes through.
            let neg = self.asm.new_label();
            self.asm.cmp_imm(TMP0, 0);
            self.asm.b_cond(C_LT, neg);
            self.asm.load_imm(TMP0, 0);
            self.asm.place(neg);
        } else {
            self.asm.bl_extern("_getcwd");
            self.asm.mov_reg(TMP0, 0);
            let done = self.asm.new_label();
            let fail = self.asm.new_label();
            self.asm.cmp_imm(TMP0, 0);
            self.asm.b_cond(C_EQ, fail);
            self.asm.load_imm(TMP0, 0); // non-NULL → 0
            self.asm.b(done);
            self.asm.place(fail);
            self.asm.bl_extern("___error");
            self.asm.ldr_w(0, 0); // w0 = errno
            self.asm.neg(TMP0, 0); // -errno
            self.asm.place(done);
        }
        if let Some(d) = dst {
            self.store_vreg(d, TMP0);
        }
        Ok(())
    }

    /// Load up to 8 primitive arguments into x0.. (in order).
    fn place_prim_args(&mut self, args: &[Val]) -> Result<(), CodegenError> {
        for (i, a) in args.iter().enumerate() {
            if i > 7 {
                return Err(self.unsupported("more than 8 primitive arguments"));
            }
            self.load_val(*a, i as u32);
        }
        Ok(())
    }

    /// The heap primitives. Freestanding: call the `mmap` bump-allocator runtime routine
    /// (`MAlloc`/`Free`/`HeapExtend`/`MSize`) via its label. Darwin: `MAlloc`→`_malloc`,
    /// `Free`→`_free`, `MSize`→0 (unsupported), `HeapExtend`→NULL.
    fn emit_heap_prim(
        &mut self,
        dst: Option<Vreg>,
        prim: Prim,
        args: &[Val],
    ) -> Result<(), CodegenError> {
        if self.ctx.freestanding {
            let name = match prim {
                Prim::MAlloc => "MAlloc",
                Prim::Free => "Free",
                Prim::HeapExtend => "HeapExtend",
                Prim::MSize => "MSize",
                _ => unreachable!(),
            };
            let label = *self
                .ctx
                .heap_labels
                .get(name)
                .ok_or_else(|| self.unsupported("heap routine not emitted"))?;
            self.place_prim_args(args)?;
            self.asm.bl(label);
            if let Some(d) = dst {
                self.store_vreg(d, 0);
            }
            return Ok(());
        }
        // Darwin.
        match prim {
            Prim::HeapExtend => {
                if let Some(d) = dst {
                    self.asm.load_imm(0, 0); // no in-place grow on hosted
                    self.store_vreg(d, 0);
                }
            }
            Prim::Free => {
                self.place_prim_args(args)?;
                self.asm.bl_extern("_free");
            }
            Prim::MAlloc => {
                self.place_prim_args(args)?;
                self.asm.bl_extern("_malloc");
                if let Some(d) = dst {
                    self.store_vreg(d, 0);
                }
            }
            Prim::MSize => {
                if let Some(d) = dst {
                    self.asm.load_imm(0, 0);
                    self.store_vreg(d, 0);
                }
            }
            _ => unreachable!(),
        }
        Ok(())
    }

    /// A clock primitive (`UnixNS`/`NanoNS`/`CpuNS`) → nanoseconds since its epoch. Reads
    /// a `timespec` on the stack via `clock_gettime` (the libc call on Darwin, the syscall
    /// freestanding) and folds it to `tv_sec * 1e9 + tv_nsec`. The clock id differs per OS
    /// (REALTIME 0/0, MONOTONIC 1/6, PROCESS_CPUTIME 2/12).
    fn emit_clock(&mut self, dst: Option<Vreg>, prim: Prim) {
        let (linux_id, macos_id): (i64, i64) = match prim {
            Prim::UnixNS => (0, 0),
            Prim::NanoNS => (1, 6),
            Prim::CpuNS => (2, 12),
            _ => unreachable!(),
        };
        self.asm.sub_sp_imm(16); // a 16-byte stack timespec (keeps 16-alignment)
        self.asm.add_imm(1, SP, 0); // x1 = &ts
        if self.ctx.freestanding {
            self.asm.load_imm(0, linux_id);
            self.asm.load_imm(SCRATCH, 113); // SYS_clock_gettime
            self.asm.svc();
        } else {
            self.asm.load_imm(0, macos_id);
            self.asm.bl_extern("_clock_gettime");
        }
        self.asm.load_mem(TMP1, SP, 8, false); // tv_sec  @ +0
        self.asm.ldur(TMP2, SP, 8); // tv_nsec @ +8
        self.asm.add_sp_imm(16);
        self.asm.load_imm(ADDR, 1_000_000_000);
        self.asm.mul(TMP1, TMP1, ADDR);
        self.asm.add(TMP0, TMP1, TMP2); // TMP0 = sec*1e9 + nsec
        if let Some(d) = dst {
            self.store_vreg(d, TMP0);
        }
    }

    /// `Sleep(ns)`: build a `timespec` (`ns/1e9`, `ns%1e9`) and `nanosleep`.
    fn emit_sleep(&mut self, args: &[Val]) -> Result<(), CodegenError> {
        if args.len() != 1 {
            return Err(self.unsupported("Sleep with other than 1 argument"));
        }
        self.load_val(args[0], TMP0); // ns
        self.asm.load_imm(ADDR, 1_000_000_000);
        self.asm.udiv(TMP1, TMP0, ADDR); // tv_sec
        self.asm.msub(TMP2, TMP1, ADDR, TMP0); // tv_nsec = ns - sec*1e9
        self.asm.sub_sp_imm(16);
        self.asm.store_mem(TMP1, SP, 8); // tv_sec  @ +0
        self.asm.stur(TMP2, SP, 8); // tv_nsec @ +8
        self.asm.add_imm(0, SP, 0); // x0 = &ts
        self.asm.load_imm(1, 0); // rem = NULL
        if self.ctx.freestanding {
            self.asm.load_imm(SCRATCH, 101); // SYS_nanosleep
            self.asm.svc();
        } else {
            self.asm.bl_extern("_nanosleep");
        }
        self.asm.add_sp_imm(16);
        Ok(())
    }

    /// `Thread(&fn, arg)` spawns a thread running `fn(arg)`; `Join(handle)` waits for it
    /// and returns its result. Hosted Darwin uses `pthread_create`/`pthread_join`;
    /// freestanding aarch64-linux uses raw `clone(2)` + a futex join (see
    /// [`Self::emit_thread_fs`]).
    ///
    /// NB: `Fs` is single-task on both arm64 targets (one shared `CTask`), so a program
    /// that throws inside **concurrently-running** threads would race on the shared
    /// exception state. Non-exception parallelism (atomics / futex locks) is correct.
    fn emit_thread(
        &mut self,
        dst: Option<Vreg>,
        prim: Prim,
        args: &[Val],
    ) -> Result<(), CodegenError> {
        if self.ctx.freestanding {
            return match prim {
                Prim::Thread => self.emit_thread_fs(dst, args),
                Prim::Join => self.emit_join_fs(dst, args),
                _ => unreachable!(),
            };
        }
        self.asm.sub_sp_imm(16); // a stack slot for the tid / retval out-param
        match prim {
            Prim::Thread => {
                self.load_val(args[1], 3); // x3 = arg
                self.load_val(args[0], 2); // x2 = start routine (function address)
                self.asm.add_imm(0, SP, 0); // x0 = &tid
                self.asm.load_imm(1, 0); // x1 = NULL attr
                self.asm.bl_extern("_pthread_create");
                self.asm.load_mem(TMP0, SP, 8, false); // TMP0 = tid (the handle)
            }
            Prim::Join => {
                self.load_val(args[0], 0); // x0 = handle
                self.asm.add_imm(1, SP, 0); // x1 = &retval
                self.asm.bl_extern("_pthread_join");
                self.asm.load_mem(TMP0, SP, 8, false); // TMP0 = the thread's return value
            }
            _ => unreachable!(),
        }
        self.asm.add_sp_imm(16);
        if let Some(d) = dst {
            self.store_vreg(d, TMP0);
        }
        Ok(())
    }

    /// Freestanding `Thread(&fn, arg)`: spawn a `CLONE_THREAD` thread via `clone(2)` onto
    /// an `mmap`'d 128 KiB region, running `fn(arg)`. A 32-byte control block at the
    /// region base — `[retval | ctid futex | fn | arg]` — carries `fn`/`arg` in and the
    /// result back; its address is the handle. `CLONE_PARENT_SETTID` writes the tid into
    /// the `ctid` futex word *synchronously* (so `Join` can't race a not-yet-set word) and
    /// `CLONE_CHILD_CLEARTID` zeroes it + futex-wakes on exit — how `Join` waits.
    ///
    /// Register-allocation-safe: nothing rides into the child in a callee-saved register
    /// (which register promotion may have claimed). The child instead recovers the base
    /// from its own `sp` (`base = sp - (STACK_SIZE - 16)`, the stack top it was cloned
    /// with), recomputing it after the call clobbers the scratch register; the parent
    /// keeps the base on its own stack across the `clone` syscall.
    fn emit_thread_fs(&mut self, dst: Option<Vreg>, args: &[Val]) -> Result<(), CodegenError> {
        const STACK_SIZE: i64 = 0x2_0000; // 128 KiB stack + control block
        // CLONE_VM|FS|FILES|SIGHAND|THREAD|PARENT_SETTID|CHILD_CLEARTID.
        const FLAGS: i64 = 0x31_0F00;

        // mmap(0, SIZE, PROT_READ|WRITE, MAP_PRIVATE|ANON, -1, 0) -> x0 = base.
        self.asm.load_imm(0, 0);
        self.asm.load_imm(1, STACK_SIZE);
        self.asm.load_imm(2, 3); // PROT_READ|PROT_WRITE
        self.asm.load_imm(3, 0x22); // MAP_PRIVATE|MAP_ANONYMOUS
        self.asm.load_imm(4, -1);
        self.asm.load_imm(5, 0);
        self.asm.load_imm(SCRATCH, 222); // SYS_mmap
        self.asm.svc();
        self.asm.mov_reg(ADDR, 0); // ADDR = base

        // Keep the base on the parent stack across the clone syscall (the handle).
        self.asm.sub_sp_imm(16);
        self.asm.str_sp(ADDR, 0); // [sp] = base
        // Control block: [base+16] = fn, [base+24] = arg.
        self.load_val(args[0], TMP0);
        self.asm.store_mem_off(TMP0, ADDR, 16, 8);
        self.load_val(args[1], TMP0);
        self.asm.store_mem_off(TMP0, ADDR, 24, 8);

        // clone(flags, child_sp, ptid=&futex, tls=0, ctid=&futex).
        let l_child = self.asm.new_label();
        let l_done = self.asm.new_label();
        self.asm.load_imm(0, FLAGS);
        self.asm.load_imm(SCRATCH, STACK_SIZE - 16);
        self.asm.add(1, ADDR, SCRATCH); // x1 = child stack top
        self.asm.add_imm(2, ADDR, 8); // x2 = ptid = &futex (set synchronously)
        self.asm.load_imm(3, 0); // x3 = tls
        self.asm.add_imm(4, ADDR, 8); // x4 = ctid = &futex (cleared + woken on exit)
        self.asm.load_imm(SCRATCH, 220); // SYS_clone
        self.asm.svc();
        self.asm.cbz(0, l_child);

        // Parent: the handle is the base. Reclaim the stack slot and finish.
        self.asm.load_mem_off(TMP0, SP, 0, 8, false);
        self.asm.add_sp_imm(16);
        self.asm.b(l_done);

        // Child: recover base from sp, run fn(arg), stash the result, exit (which fires
        // CLONE_CHILD_CLEARTID, waking a joiner).
        self.asm.place(l_child);
        self.asm.add_imm(TMP1, SP, 0); // TMP1 = sp = child stack top
        self.asm.load_imm(SCRATCH, STACK_SIZE - 16);
        self.asm.sub(TMP1, TMP1, SCRATCH); // TMP1 = base
        self.asm.load_mem_off(TMP0, TMP1, 16, 8, false); // fn
        self.asm.load_mem_off(0, TMP1, 24, 8, false); // x0 = arg
        self.asm.blr(TMP0); // x0 = fn(arg); the call clobbers TMP1
        self.asm.add_imm(TMP1, SP, 0); // recompute base from the (restored) sp
        self.asm.load_imm(SCRATCH, STACK_SIZE - 16);
        self.asm.sub(TMP1, TMP1, SCRATCH);
        self.asm.store_mem_off(0, TMP1, 0, 8); // [base+0] = retval
        self.asm.load_imm(0, 0);
        self.asm.load_imm(SCRATCH, 93); // SYS_exit (this thread)
        self.asm.svc();

        self.asm.place(l_done);
        if let Some(d) = dst {
            self.store_vreg(d, TMP0); // parent: handle = base
        }
        Ok(())
    }

    /// Freestanding `Join(handle)`: futex-wait on the control block's `ctid` word until
    /// the kernel clears it (thread exit), then return the `retval` the thread left. The
    /// base is held on the stack across the futex syscall so no callee-saved register
    /// (possibly promoted) is needed.
    fn emit_join_fs(&mut self, dst: Option<Vreg>, args: &[Val]) -> Result<(), CodegenError> {
        self.load_val(args[0], TMP1); // base (handle)
        self.asm.sub_sp_imm(16);
        self.asm.str_sp(TMP1, 0); // [sp] = base

        let l_wait = self.asm.new_label();
        let l_done = self.asm.new_label();
        self.asm.place(l_wait);
        self.asm.load_mem_off(TMP1, SP, 0, 8, false); // base
        self.asm.load_mem_off(TMP0, TMP1, 8, 8, false); // *ctid (0 once the thread exits)
        self.asm.cbz(TMP0, l_done);
        // futex(&ctid, FUTEX_WAIT=0, val=*ctid, timeout=NULL).
        self.asm.add_imm(0, TMP1, 8); // uaddr
        self.asm.load_imm(1, 0); // FUTEX_WAIT
        self.asm.mov_reg(2, TMP0); // val = the tid we observed
        self.asm.load_imm(3, 0); // timeout = NULL
        self.asm.load_imm(SCRATCH, 98); // SYS_futex
        self.asm.svc();
        self.asm.b(l_wait);

        self.asm.place(l_done);
        self.asm.load_mem_off(TMP1, SP, 0, 8, false); // base
        self.asm.load_mem_off(TMP0, TMP1, 0, 8, false); // [base+0] = retval
        self.asm.add_sp_imm(16);
        if let Some(d) = dst {
            self.store_vreg(d, TMP0);
        }
        Ok(())
    }

    /// An atomic op (`stdatomic.hc`), width-directed by `width` (the pointee type).
    /// Load/store use `ldar`/`stlr`; add/swap/cas use `ldaxr`/`stlxr` retry loops. The
    /// witnessed/result value is sign/zero-extended to the pointee width so it matches a
    /// normal load. Mirrors the AST backend's `gen_atomic`.
    fn emit_atomic(
        &mut self,
        dst: Option<Vreg>,
        prim: Prim,
        args: &[Val],
        width: IrTy,
    ) -> Result<(), CodegenError> {
        let sz = match width.size() {
            1 => 0,
            2 => 1,
            4 => 2,
            _ => 3,
        };
        match prim {
            Prim::AtomicLoad => {
                self.load_val(args[0], ADDR);
                self.asm.ldar(TMP0, ADDR, sz);
                self.emit_int_cast(width);
            }
            Prim::AtomicStore => {
                self.load_val(args[0], ADDR);
                self.load_val(args[1], TMP0);
                self.asm.stlr(TMP0, ADDR, sz);
            }
            Prim::AtomicAdd => {
                self.load_val(args[0], ADDR);
                self.load_val(args[1], TMP2); // delta
                let l = self.asm.new_label();
                self.asm.place(l);
                self.asm.ldaxr(TMP0, ADDR, sz); // old
                self.emit_int_cast(width); // extend old (correct add for a narrow type)
                self.asm.add(TMP0, TMP0, TMP2); // new = old + delta
                self.asm.stlxr(TMP1, TMP0, ADDR, sz);
                self.asm.cbnz(TMP1, l);
                self.emit_int_cast(width); // extend the stored-width result
            }
            Prim::AtomicSwap => {
                self.load_val(args[0], ADDR);
                self.load_val(args[1], TMP2); // new value
                let l = self.asm.new_label();
                self.asm.place(l);
                self.asm.ldaxr(TMP0, ADDR, sz); // old (the result)
                self.asm.stlxr(TMP1, TMP2, ADDR, sz);
                self.asm.cbnz(TMP1, l);
                self.emit_int_cast(width);
            }
            Prim::AtomicCas => {
                self.load_val(args[0], ADDR);
                self.load_val(args[1], TMP2); // expected
                self.load_val(args[2], SCRATCH); // desired
                let l = self.asm.new_label();
                let done = self.asm.new_label();
                self.asm.place(l);
                self.asm.ldaxr(TMP0, ADDR, sz); // old (witnessed)
                self.emit_int_cast(width);
                self.asm.cmp_reg(TMP0, TMP2);
                self.asm.b_cond(C_NE, done); // mismatch → return old, no store
                self.asm.stlxr(TMP1, SCRATCH, ADDR, sz);
                self.asm.cbnz(TMP1, l); // lost the monitor → retry
                self.asm.place(done);
            }
            _ => unreachable!(),
        }
        if let Some(d) = dst {
            self.store_vreg(d, TMP0);
        }
        Ok(())
    }

    /// `FutexWait(addr, val)` / `FutexWake(addr, n)`. Freestanding uses the Linux
    /// `futex(2)` syscall; Darwin uses libc `__ulock_wait`/`__ulock_wake`. A `FutexWait`
    /// carries a short timeout so a missed wakeup degrades to a periodic re-check.
    fn emit_futex(&mut self, prim: Prim, args: &[Val]) -> Result<(), CodegenError> {
        let wake = matches!(prim, Prim::FutexWake);
        const FUTEX_TIMEOUT_NS: i64 = 1_000_000;
        if self.ctx.freestanding {
            self.load_val(args[0], 0); // x0 = uaddr
            self.load_val(args[1], 2); // x2 = val (expected / n)
            self.asm.load_imm(1, if wake { 1 } else { 0 }); // FUTEX_WAKE / FUTEX_WAIT
            if wake {
                self.asm.load_imm(3, 0); // no timeout
            } else {
                self.asm.sub_sp_imm(16); // relative timespec {0, TIMEOUT} on the stack
                self.asm.load_imm(TMP0, 0);
                self.asm.str_sp(TMP0, 0); // tv_sec
                self.asm.load_imm(TMP0, FUTEX_TIMEOUT_NS);
                self.asm.str_sp(TMP0, 8); // tv_nsec
                self.asm.add_imm(3, SP, 0); // x3 = &timespec
            }
            self.asm.load_imm(4, 0); // uaddr2
            self.asm.load_imm(5, 0); // val3
            self.asm.load_imm(SCRATCH, 98); // SYS_futex
            self.asm.svc();
            if !wake {
                self.asm.add_sp_imm(16);
            }
        } else {
            self.load_val(args[0], 1); // x1 = addr
            self.load_val(args[1], 2); // x2 = value (ignored for wake)
            if wake {
                self.asm.load_imm(2, 0); // wake one
                self.asm.load_imm(3, 0);
            } else {
                self.asm.load_imm(3, FUTEX_TIMEOUT_NS / 1000); // timeout µs
            }
            self.asm.load_imm(0, 1); // UL_COMPARE_AND_WAIT
            self.asm.bl_extern(if wake {
                "___ulock_wake"
            } else {
                "___ulock_wait"
            });
        }
        Ok(())
    }

    /// Freestanding primitives backed by raw `aarch64` Linux syscalls (args in x0.., the
    /// number in x8, `svc`; the kernel returns the result or `-errno` in x0).
    fn emit_syscall_prim(
        &mut self,
        dst: Option<Vreg>,
        prim: Prim,
        args: &[Val],
    ) -> Result<(), CodegenError> {
        let nr: i64 = match prim {
            Prim::StdWrite | Prim::Write => 64, // write
            Prim::Read => 63,                   // read
            Prim::Close => 57,                  // close
            Prim::LSeek => 62,                  // lseek
            Prim::Socket => 198,                // socket
            Prim::Connect => 203,               // connect
            Prim::Exit => 94,                   // exit_group
            other => {
                return Err(self.unsupported(&format!("freestanding primitive {other:?}")));
            }
        };
        self.place_prim_args(args)?;
        self.asm.load_imm(SCRATCH, nr); // x8 = syscall number
        self.asm.svc();
        if let Some(d) = dst {
            self.store_vreg(d, 0);
        }
        Ok(())
    }

    /// `Open(path, flags, mode)`. Freestanding: `openat(AT_FDCWD, path, flags, mode)` —
    /// aarch64 has no bare `open`, the `fcntl.hc` flags are already Linux's, and the
    /// syscall returns `-errno` directly. Hosted Darwin: translate the Linux-canonical
    /// `O_*` flags to macOS, call the variadic libc `open` (the `mode` arg travels on the
    /// stack), sign-extend the `int` result, and convert a `-1` failure into the
    /// `-errno` (Linux-normalised) the rest of the stdlib returns.
    fn emit_open(&mut self, dst: Option<Vreg>, args: &[Val]) -> Result<(), CodegenError> {
        if args.len() != 3 {
            return Err(self.unsupported("Open with other than 3 arguments"));
        }
        if self.ctx.freestanding {
            self.load_val(args[2], 3); // x3 = mode
            self.load_val(args[1], 2); // x2 = flags (Linux values, verbatim)
            self.load_val(args[0], 1); // x1 = path
            self.asm.load_imm(0, -100); // x0 = AT_FDCWD
            self.asm.load_imm(SCRATCH, 56); // x8 = SYS_openat
            self.asm.svc();
            if let Some(d) = dst {
                self.store_vreg(d, 0); // fd / -errno
            }
            return Ok(());
        }
        self.load_val(args[0], 0); // x0 = path
        self.load_val(args[1], 1); // x1 = flags (Linux values)
        // macos = (f & 3) | (O_CREAT 0x40→0x200) | (O_TRUNC 0x200→0x400) |
        //         (O_APPEND 0x400→0x8): move each `from`-bit to its `to`-bit.
        self.asm.and_imm_lowbits(TMP2, 1, 2); // access mode (low 2 bits)
        for (from, to) in [(6u32, 9u32), (9, 10), (10, 3)] {
            self.asm.lsr_imm(TMP0, 1, from);
            self.asm.and_imm_lowbits(TMP0, TMP0, 1);
            self.asm.lsl_imm(TMP0, TMP0, to);
            self.asm.orr(TMP2, TMP2, TMP0);
        }
        self.asm.mov_reg(1, TMP2); // x1 = translated flags
        self.load_val(args[2], SCRATCH); // mode (the first stack vararg)
        self.asm.sub_sp_imm(16);
        self.asm.str_sp(SCRATCH, 0); // [sp] = mode
        self.asm.bl_extern("_open");
        self.asm.add_sp_imm(16);
        self.asm.mov_reg(TMP0, 0); // result in TMP0
        self.emit_int_cast(IrTy::I32); // sign-extend the libc `int`
        self.emit_errno_neg(); // -1 → -errno (normalised)
        if let Some(d) = dst {
            self.store_vreg(d, TMP0);
        }
        Ok(())
    }

    /// After a libc call whose `int` result is in `TMP0`, convert a `-1` failure to the
    /// `-errno` the freestanding syscalls return: `if (TMP0 < 0) TMP0 = -*___error();`,
    /// with the Darwin errno normalised to its Linux-canonical value (the same table the
    /// interpreter uses, so they can't drift). Mirrors the AST `darwin_errno_neg`.
    fn emit_errno_neg(&mut self) {
        let ok = self.asm.new_label();
        self.asm.cmp_imm(TMP0, 0);
        self.asm.b_cond(C_GE, ok);
        self.asm.bl_extern("___error");
        self.asm.ldr_w(0, 0); // w0 = errno (Darwin numbering)
        let done = self.asm.new_label();
        for &(darwin, linux) in crate::intrinsics::DARWIN_TO_LINUX_ERRNO {
            let next = self.asm.new_label();
            self.asm.cmp_imm(0, darwin as u32);
            self.asm.b_cond(C_NE, next);
            self.asm.load_imm(0, linux);
            self.asm.b(done);
            self.asm.place(next);
        }
        self.asm.place(done);
        self.asm.neg(TMP0, 0); // TMP0 = -errno
        self.asm.place(ok);
    }

    fn emit_term(&mut self, term: &IrTerm, _f: &IrFunc) -> Result<(), CodegenError> {
        match term {
            IrTerm::Br(t) => self.asm.b(self.block_labels[*t as usize]),
            IrTerm::CondBr { cond, t, f } => {
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
            IrTerm::Switch {
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
            IrTerm::Ret(v) => {
                match v {
                    // A float result returns in d0; everything else in x0.
                    Some(val) if matches!(self.ret, IrRet::Scalar(t) if t.is_float()) => {
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
            IrTerm::Throw(_) | IrTerm::Rethrow => self.emit_unwind(),
            IrTerm::Unreachable => self.epilogue(),
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

    // ---- exceptions (jmp_buf/longjmp-style unwind over `Fs->exc_top`) ----
    //
    // An `ExcFrame` (32 bytes) is `{ prev, saved_sp, saved_fp, landing_pad }`. The
    // spill-everything model keeps nothing in callee-saved registers across a `try`, so —
    // unlike the AST backend — no callee-saved set is saved here. Scratch regs TMP0/TMP1/
    // TMP2 are caller-saved and reloaded per instruction, so they are free to clobber.

    /// Load the current task pointer (`*Fs`, a `CTask *`) into `reg`.
    fn fs_ptr(&mut self, reg: u32) {
        let fs = self
            .ctx
            .fs_gid
            .expect("Fs accessed in a program without the Fs global");
        self.global_addr_into(reg, fs, 0);
        self.asm.load_mem(reg, reg, 8, false); // reg = *(&Fs) = CTask*
    }

    /// `TryBegin`: build the on-stack `ExcFrame` and push it onto `Fs->exc_top`.
    fn emit_try_begin(&mut self, pad: BlockId, frame: SlotId) {
        let exc_top = self.ctx.exc_top_off;
        self.slot_addr(frame, 0, TMP1); // TMP1 = &ExcFrame
        self.fs_ptr(TMP2); // TMP2 = CTask*
        // frame.prev = Fs->exc_top
        self.asm.load_mem_off(TMP0, TMP2, exc_top, 8, false);
        self.asm.store_mem_off(TMP0, TMP1, 0, 8);
        // frame.saved_sp = sp ; frame.saved_fp = x29
        self.asm.add_imm(TMP0, SP, 0);
        self.asm.store_mem_off(TMP0, TMP1, 8, 8);
        self.asm.store_mem_off(FP, TMP1, 16, 8);
        // frame.landing_pad = &pad
        self.asm.adr_label(TMP0, self.block_labels[pad as usize]);
        self.asm.store_mem_off(TMP0, TMP1, 24, 8);
        // Fs->exc_top = &ExcFrame
        self.asm.store_mem_off(TMP1, TMP2, exc_top, 8);
    }

    /// `TryEnd`: normal completion pops the handler (`Fs->exc_top = Fs->exc_top->prev`).
    fn emit_try_end(&mut self) {
        let exc_top = self.ctx.exc_top_off;
        self.fs_ptr(TMP2); // CTask*
        self.asm.load_mem_off(TMP0, TMP2, exc_top, 8, false); // top
        self.asm.load_mem_off(TMP1, TMP0, 0, 8, false); // top->prev
        self.asm.store_mem_off(TMP1, TMP2, exc_top, 8);
    }

    /// `Throw`/`Rethrow`: unwind to the nearest handler — restore its sp/fp from the top
    /// `ExcFrame`, pop it, and branch to its landing pad. An empty chain is an uncaught
    /// exception, which exits with the thrown value (`Fs->except_ch`) as the code.
    fn emit_unwind(&mut self) {
        let exc_top = self.ctx.exc_top_off;
        self.fs_ptr(TMP2); // TMP2 = CTask*
        self.asm.load_mem_off(TMP1, TMP2, exc_top, 8, false); // TMP1 = top frame
        let live = self.asm.new_label();
        self.asm.cbnz(TMP1, live);
        // Uncaught: exit(Fs->except_ch) (the thrown value the lowering already stored).
        self.asm
            .load_mem_off(0, TMP2, self.ctx.except_ch_off, 8, false);
        if self.ctx.freestanding {
            self.asm.load_imm(SCRATCH, 94); // x8 = SYS_exit_group
            self.asm.svc();
        } else {
            self.asm.bl_extern("_exit");
        }
        self.asm.place(live);
        // Fs->exc_top = top->prev
        self.asm.load_mem_off(TMP0, TMP1, 0, 8, false);
        self.asm.store_mem_off(TMP0, TMP2, exc_top, 8);
        // Restore sp then fp from the frame, then branch to its landing pad.
        self.asm.load_mem_off(TMP0, TMP1, 8, 8, false); // saved_sp
        self.asm.add_imm(SP, TMP0, 0);
        self.asm.load_mem_off(FP, TMP1, 16, 8, false); // saved_fp
        self.asm.load_mem_off(IND, TMP1, 24, 8, false); // landing_pad
        self.asm.br(IND);
    }

    fn epilogue(&mut self) {
        // Restore the caller's callee-saved registers (FP still valid, so addressing is
        // `FP - off`), then tear down the frame.
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
}

/// Whether `f` touches the per-task `Fs` — it accesses the `Fs` global (`Fs->field`) or
/// has any exception op (`try`/`throw`). Such a function needs a cached `CTask*` slot.
fn func_uses_fs(f: &IrFunc, fs_gid: GlobalId) -> bool {
    f.blocks.iter().any(|b| {
        matches!(b.term, IrTerm::Throw(_) | IrTerm::Rethrow)
            || b.insts.iter().any(|i| match i {
                IrInst::TryBegin { .. } | IrInst::TryEnd => true,
                IrInst::GlobalAddr { global, .. } => *global == fs_gid,
                _ => false,
            })
    })
}

// ---- freestanding heap runtime (mmap bump allocator) ----
//
// These mirror the AST backend's `emit_fs_*` routines verbatim (one `mmap`-backed bump
// allocator, 16-byte-aligned allocations, two BSS state words for the bump pointer and
// chunk end; `Free` is a no-op so chunks are never reused). They emit raw `Asm` and are
// called via `bl` from `emit_heap_prim`. `hp`/`he` are the BSS offsets of the bump
// pointer and chunk end; `uses_msize` reserves an 8-byte size header per block.

const HS_HI: u32 = 0b1000; // unsigned higher (>)
const HS_LS: u32 = 0b1001; // unsigned lower-or-same (<=)
const HS_HS: u32 = 0b0010; // unsigned higher-or-same (>=)
const HS_NE: u32 = 0b0001;

/// Emit the heap routines the program calls, each at its label.
fn emit_heap_runtime(
    asm: &mut Asm,
    labels: &HashMap<&'static str, usize>,
    hp: u64,
    he: u64,
    uses_msize: bool,
) {
    if let Some(&l) = labels.get("MAlloc") {
        asm.place(l);
        emit_fs_malloc(asm, hp, he, uses_msize);
    }
    if let Some(&l) = labels.get("HeapExtend") {
        asm.place(l);
        emit_fs_heapextend(asm, hp, he, uses_msize);
    }
    if let Some(&l) = labels.get("MSize") {
        asm.place(l);
        emit_fs_msize(asm);
    }
    if let Some(&l) = labels.get("Free") {
        asm.place(l);
        asm.ret(); // a no-op bump allocator never frees
    }
}

/// `MAlloc(x0=n) -> x0`: a bump allocator over `mmap`'d chunks (≥1 MiB, page-aligned).
fn emit_fs_malloc(asm: &mut Asm, hp: u64, he: u64, uses_msize: bool) {
    let fits = asm.new_label();
    let sized = asm.new_label();
    if uses_msize {
        asm.push(0); // save the original n for the size header
    }
    // x9 = (n + 15) & ~15
    asm.add_imm(9, 0, 15);
    asm.load_imm(10, -16);
    asm.and(9, 9, 10);
    if uses_msize {
        asm.add_imm(9, 9, 16); // reserve a 16-byte size header
    }
    // x11 = *hp, x12 = *he
    asm.adr_global_fs(13, hp);
    asm.load_mem(11, 13, 8, false);
    asm.adr_global_fs(14, he);
    asm.load_mem(12, 14, 8, false);
    asm.add(15, 11, 9); // hp + n
    asm.cmp_reg(15, 12);
    asm.b_cond(HS_LS, fits); // fits in the current chunk
    // chunk size x1 = max(n, 1 MiB), rounded up to a page
    asm.mov_reg(1, 9);
    asm.load_imm(10, 0x10_0000);
    asm.cmp_reg(1, 10);
    asm.b_cond(HS_HS, sized);
    asm.mov_reg(1, 10);
    asm.place(sized);
    asm.add_imm(1, 1, 4095);
    asm.load_imm(10, -4096);
    asm.and(1, 1, 10);
    // mmap(0, x1, PROT_READ|WRITE=3, MAP_PRIVATE|ANON=0x22, -1, 0), nr 222.
    asm.load_imm(0, 0);
    asm.load_imm(2, 3);
    asm.load_imm(3, 0x22);
    asm.load_imm(4, -1);
    asm.load_imm(5, 0);
    asm.load_imm(8, 222);
    asm.svc();
    asm.mov_reg(11, 0); // hp base = mmap base
    asm.add(12, 0, 1); // he = base + chunk size
    asm.adr_global_fs(14, he);
    asm.store_mem(12, 14, 8);
    asm.place(fits);
    // result = x11 (base); *hp = base + n
    asm.add(15, 11, 9);
    asm.adr_global_fs(13, hp);
    asm.store_mem(15, 13, 8);
    if uses_msize {
        asm.pop(10); // x10 = original n
        asm.store_mem(10, 11, 8); // [base] = n (the size header)
        asm.add_imm(0, 11, 16); // return base + 16 (past the header)
    } else {
        asm.mov_reg(0, 11);
    }
    asm.ret();
}

/// `HeapExtend(x0=ptr, x1=old, x2=new) -> x0`: grow `ptr` in place when it is the last
/// bump-allocated block and still fits the chunk; else NULL.
fn emit_fs_heapextend(asm: &mut Asm, hp: u64, he: u64, uses_msize: bool) {
    let null = asm.new_label();
    asm.cbz(0, null); // NULL ptr never extends
    // x9 = align16(old), x11 = align16(new)
    asm.add_imm(9, 1, 15);
    asm.load_imm(10, -16);
    asm.and(9, 9, 10);
    asm.add_imm(11, 2, 15);
    asm.and(11, 11, 10);
    // last block? ptr + align16(old) == *heap_ptr
    asm.add(12, 0, 9);
    asm.adr_global_fs(13, hp);
    asm.load_mem(14, 13, 8, false);
    asm.cmp_reg(12, 14);
    asm.b_cond(HS_NE, null);
    // fits? ptr + align16(new) <= *heap_end
    asm.add(12, 0, 11);
    asm.adr_global_fs(14, he);
    asm.load_mem(15, 14, 8, false);
    asm.cmp_reg(12, 15);
    asm.b_cond(HS_HI, null); // ptr+anew > heap_end ⇒ doesn't fit
    // extend in place: *heap_ptr = ptr + anew; return ptr (x0 unchanged)
    asm.store_mem(12, 13, 8);
    if uses_msize {
        asm.sub_imm(9, 0, 16); // x9 = ptr - 16 (header)
        asm.store_mem(2, 9, 8); // keep MSize current: [ptr-16] = new size
    }
    asm.ret();
    asm.place(null);
    asm.load_imm(0, 0); // NULL
    asm.ret();
}

/// `MSize(x0=ptr) -> x0`: the requested byte size from `ptr`'s header (`*(ptr-16)`).
fn emit_fs_msize(asm: &mut Asm) {
    let null = asm.new_label();
    asm.cbz(0, null);
    asm.sub_imm(9, 0, 16);
    asm.load_mem(0, 9, 8, false); // x0 = *(ptr - 16)
    asm.ret();
    asm.place(null);
    asm.load_imm(0, 0); // MSize(NULL) == 0
    asm.ret();
}

/// Native end-to-end tests for the IR backend: compile via `compile_ir`, link with
/// `cc`, run, and compare to the tree-walking oracle. Apple-silicon macOS only (the
/// one host that can both emit and execute these), self-skipping elsewhere.
#[cfg(all(test, target_arch = "aarch64", target_os = "macos"))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn ir_native_output(src: &str) -> String {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let prog = crate::parser::parse(src).expect("parse");
        // Sema annotates `e.ty()`, which the type-directed lowering needs (the CLI runs
        // it before codegen).
        assert!(crate::sema::check_program(&prog).is_empty(), "sema: {src}");
        let obj = compile_ir(&prog, &super::super::darwin::Darwin)
            .unwrap_or_else(|e| panic!("compile_ir failed for {src:?}: {e}"));
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir();
        let obj_path = dir.join(format!("solomon-ir-{}-{n}.o", std::process::id()));
        let exe_path = dir.join(format!("solomon-ir-{}-{n}", std::process::id()));
        std::fs::write(&obj_path, &obj).expect("write obj");
        super::super::darwin::Darwin
            .link(&obj_path, &exe_path)
            .expect("link");
        let out = std::process::Command::new(&exe_path).output().expect("run");
        let _ = std::fs::remove_file(&obj_path);
        let _ = std::fs::remove_file(&exe_path);
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    #[test]
    fn ir_backend_matches_oracle() {
        let sources = [
            // strings / control flow / globals
            "\"Hello, world!\\n\";",
            "I64 i; for (i = 0; i < 5; i++) if (i & 1) \"odd\\n\"; else \"even\\n\";",
            // calls, recursion, struct-by-value (sret)
            "I64 fib(I64 n) { if (n < 2) return n; return fib(n-1) + fib(n-2); } \
             U0 M() { if (fib(10) == 55) \"fib ok\\n\"; } M;",
            "class P { I64 x; I64 y; } P mk(I64 a, I64 b) { P p; p.x=a; p.y=b; return p; } \
             U0 M() { P q = mk(3, 4); \"%d\\n\", q.x + q.y; } M;",
            // the printf path: integers, hex/unsigned, width, and floats
            "\"%d + %d = %d\\n\", 2, 3, 2 + 3;",
            "\"%x %u %d\\n\", 255, 7, -7;",
            "\"[%5d][%-5d][%05d]\\n\", 42, 42, 42;",
            "\"pi=%f e=%g\\n\", 3.14159, 2.71828;",
            "I64 i; for (i = 1; i <= 5; i++) \"i=%d sq=%d\\n\", i, i * i;",
            // exceptions: catch a local throw, a throw that unwinds out of a callee, and
            // a nested bare re-raise (the full ExcFrame push/pop + longjmp unwind path).
            "try { throw(42); \"unreached\\n\"; } catch { \"caught %d\\n\", Fs->except_ch; }",
            "I64 Boom(I64 n) { if (n > 3) throw(n * 10); return n; } \
             U0 M() { try { Boom(9); \"no\\n\"; } catch { \"got %d\\n\", Fs->except_ch; } } M;",
            "try { try { throw(7); } catch { \"inner %d\\n\", Fs->except_ch; throw; } } \
             catch { \"outer %d flag=%d\\n\", Fs->except_ch, Fs->catch_except; } \
             \"flag now %d\\n\", Fs->catch_except;",
            // the command line (run with no args ⇒ argc == 1, matching the oracle's
            // default argv) and an fd primitive with errno conversion.
            "\"argc=%d\\n\", ArgC; if (ArgC >= 1) \"have prog name\\n\";",
            "#include <fcntl.hc>\n\
             I64 fd = Open(\"/no_such_solomon_4c1f_file\", O_RDONLY, 0); \
             \"missing=%d\\n\", fd < 0;",
        ];
        for src in sources {
            let prog = crate::parser::parse(src).expect("parse");
            let oracle = crate::interp::run_to_string(&prog).expect("oracle");
            assert_eq!(
                ir_native_output(src),
                oracle,
                "IR backend differs for:\n{src}"
            );
        }
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
