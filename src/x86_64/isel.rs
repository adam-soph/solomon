//! x86-64 instruction selection — the x86 backend's machine-code generator.
//!
//! This is **not** an IR; it *consumes* the one SSA [IR](crate::ir) (shared with the
//! interpreter and the arm64 backend) and emits x86-64. The block-walk driver and the
//! pure-IR analyses are shared via [`crate::backend`]; this module supplies the per-arch
//! instruction selection, ABI, exception unwind, primitives, and `Asm` encoding.
//!
//! It lowers a program to the SSA [IR](crate::ir), destructs it out of SSA
//! ([`crate::regalloc::destruct_program`]), and emits x86-64 by walking the resulting
//! `phi`-free blocks — reusing the [`Asm`](super::asm::Asm) encoder, the freestanding-ELF
//! writer (Linux), and the kernel32-import PE writer (Windows), both behind the
//! [`OsTarget`](super::OsTarget) seam. It replaces the old AST-walking codegen and the
//! shared `backend.rs` drivers: control flow and initializers are already lowered into IR
//! blocks/terminators, so this backend only does instruction selection.
//!
//! One [`OsTarget`] selects the per-OS deltas: process exit, page allocation (the heap's
//! fresh-chunk grab), the stdout sink, file ops, the clock, the command-line/env capture,
//! and the container format. Everything else is OS-agnostic x86-64.
//!
//! Register model: **spill-everything**. Every SSA value lives in an `[rbp - off]` frame
//! slot; operands are loaded into scratch GPRs (rax/rcx/rdx, plus rsi/rdi for addresses)
//! or xmm0/xmm1 for F64, combined, and stored back. The internal ABI matches the arm64
//! backend's: int/ptr args in rdi/rsi/rdx/rcx/r8/r9, F64 in xmm0–7 (counted independently),
//! class-by-value carried by address, an aggregate return via an sret pointer in r11.
//! `Fs` (the exception task) is a single BSS `CTask` whose pointer is seeded into the `Fs`
//! global at `@entry` — single-task, matching the IR interpreter oracle; the `ExcFrame` is
//! `{prev, saved_rsp, saved_rbp, landing_pad}` (32 bytes, no callee-saved set, since
//! nothing is promoted). The algebraic intrinsics `Sqrt`/`Fabs` lower to `sqrtsd`/`andpd`.

use std::collections::HashMap;

use super::asm::Asm;
use super::{
    FileOp, OsTarget, R8, R9, R10, R11, R12, R13, R14, RAX, RBP, RBX, RCX, RDI, RDX, RSI, RSP,
    align16,
};
use crate::codegen::CodegenError;
use crate::ir::*;
use crate::regalloc::PReg;

// Scratch GPRs (all caller-saved, all low registers 0–7 so the parametric frame
// load/store encoders need no REX.R). Values are reloaded from slots per instruction.
const TMP0: u8 = RAX; // 0 — primary value scratch / result register
const TMP1: u8 = RCX; // 1 — secondary scratch / shift count (cl)
const ADDR: u8 = RSI; // 6 — address scratch
const ADDR2: u8 = RDI; // 7 — second address scratch (rep movsb dest)

// System V integer argument registers, in order.
const ARG_GPR: [u8; 6] = [RDI, RSI, RDX, RCX, R8, R9];

// The callee-saved GPRs hot vregs may be promoted into. r15 is excluded — the Windows
// `OsTarget` seam uses it to save rsp around aligned calls. System V has no callee-saved
// xmm, so the float pool is empty: only integer/pointer vregs are promoted.
const PROMOTE_INT: [u32; 4] = [RBX as u32, R12 as u32, R13 as u32, R14 as u32];

// `setcc` opcode second bytes.
const SETE: u8 = 0x94;
const SETNE: u8 = 0x95;
const SETL: u8 = 0x9C;
const SETGE: u8 = 0x9D;
const SETLE: u8 = 0x9E;
const SETG: u8 = 0x9F;
const SETB: u8 = 0x92;
const SETAE: u8 = 0x93;
const SETBE: u8 = 0x96;
const SETA: u8 = 0x97;

/// Compile `program` through the IR pipeline to a runnable executable image, with the
/// OS-specific steps (exit, page alloc, stdout, file ops, clock, container) deferred to
/// `os`.
pub(super) fn compile_ir(
    program: &crate::ast::Program,
    mut os: Box<dyn OsTarget>,
) -> Result<Vec<u8>, CodegenError> {
    let (layouts, _) = crate::layout::compute(program);
    let ir = crate::lower::lower(program, &layouts)?;
    let ir = crate::regalloc::destruct_program(&ir);

    // Reachable functions from `@entry`, over direct calls and `&Func`, `@entry` first
    // (it is the program entry point — the first code byte).
    let reachable = crate::backend::reachable_functions(&ir, "x86_64 backend")?;

    let gid_of = |name: &str| {
        ir.globals
            .iter()
            .position(|g| g.name == name)
            .map(|i| i as u32)
    };
    let fs_gid = gid_of("Fs");
    let (exc_top_off, except_ch_off) = if fs_gid.is_some() {
        let field = |name: &str| {
            ir.layouts.offset_of("CTask", name).ok_or_else(|| {
                CodegenError::new(format!("IR x86_64 backend: CTask has no {name}"), None)
            })
        };
        (field("exc_top")? as i32, field("except_ch")? as i32)
    } else {
        (0, 0)
    };
    let ctask_size = ir
        .layouts
        .size_of(&crate::ast::Type::Named("CTask".to_string()))
        .max(8) as i32;
    // `Fs` is registered unconditionally by lowering (to match the interpreter), but a
    // program that never touches it or throws needs neither the `CTask` region nor the
    // `Fs` pointer slot — gate both on real use so a non-exception program stays lean.
    let prog_uses_fs =
        fs_gid.is_some_and(|g| reachable.iter().any(|f| crate::backend::func_uses_fs(f, g)));

    // Which impure primitive groups the reachable code uses, to size BSS and emit only the
    // heap routines actually needed. (Heap set is shared; the clock scratch is x86-only.)
    let heap_used = crate::backend::heap_prims_used(&reachable);
    let uses_clock = reachable.iter().flat_map(|f| &f.blocks).any(|b| {
        b.insts.iter().any(|i| {
            matches!(
                i,
                IrInst::Prim {
                    prim: Prim::UnixNS | Prim::NanoNS | Prim::CpuNS | Prim::Sleep,
                    ..
                }
            )
        })
    });
    let uses_msize = heap_used.contains("MSize");

    let mut asm = Asm::new();
    // One code label per reachable function; one per used heap routine.
    let labels: HashMap<&str, usize> = reachable
        .iter()
        .map(|f| (f.name.as_str(), asm.new_label()))
        .collect();
    let mut heap_labels: HashMap<&'static str, usize> = HashMap::new();
    for &name in &["MAlloc", "Free", "HeapExtend", "MSize"] {
        if heap_used.contains(name) {
            heap_labels.insert(name, asm.new_label());
        }
    }

    // Intern every string literal once (NUL-terminated); map IR string ids to indices.
    let str_idx: Vec<usize> = ir
        .strings
        .iter()
        .map(|bytes| {
            let mut b = bytes.clone();
            if b.last() != Some(&0) {
                b.push(0);
            }
            asm.intern(&b)
        })
        .collect();

    // BSS layout: every global, the CTask region, the heap bump words, a clock scratch.
    let mut bss = 0i32;
    let mut alloc_bss = |size: i32, align: i32| {
        let a = align.max(1);
        let off = (bss + a - 1) / a * a;
        bss = off + size.max(1);
        off
    };
    let global_bss: Vec<i32> = ir
        .globals
        .iter()
        .enumerate()
        .map(|(i, g)| {
            // The unused `Fs` slot gets a dummy offset (never referenced).
            if Some(i as u32) == fs_gid && !prog_uses_fs {
                0
            } else {
                alloc_bss(g.size as i32, g.align as i32)
            }
        })
        .collect();
    let ctask_off = prog_uses_fs.then(|| alloc_bss(ctask_size, 16));
    let heap_words = if heap_used.is_empty() {
        None
    } else {
        Some((alloc_bss(8, 8), alloc_bss(8, 8))) // (heap_ptr, heap_end)
    };
    let clock_scratch = if uses_clock {
        Some(alloc_bss(32, 8))
    } else {
        None
    };

    // The command-line capture writes *both* argc and argv, but lowering registers each
    // global only when that exact name is used. So when either is used, give the missing
    // one a write-only scratch slot (the program never reads it).
    let need_args = gid_of("argc").is_some() || gid_of("argv").is_some();
    let argc_off = need_args.then(|| {
        gid_of("argc")
            .map(|g| global_bss[g as usize])
            .unwrap_or_else(|| alloc_bss(8, 8))
    });
    let argv_off = need_args.then(|| {
        gid_of("argv")
            .map(|g| global_bss[g as usize])
            .unwrap_or_else(|| alloc_bss(8, 8))
    });
    let envp_off = gid_of("envp").map(|g| global_bss[g as usize]);
    let ctx = Ctx {
        global_bss,
        fs_gid,
        ctask_off,
        exc_top_off,
        except_ch_off,
        heap_labels,
        clock_scratch,
        argc_off,
        argv_off,
        envp_off,
    };

    for f in &reachable {
        let is_entry = f.name == crate::lower::ENTRY;
        let mut e = FnEmit {
            asm: &mut asm,
            os: os.as_mut(),
            ctx: &ctx,
            labels: &labels,
            str_idx: &str_idx,
            ret: f.ret,
            block_labels: Vec::new(),
            slot_off: Vec::new(),
            vreg_off: Vec::new(),
            vreg_reg: Vec::new(),
            saved_regs: Vec::new(),
            is_entry,
        };
        e.emit(f)?;
    }

    // The heap runtime (only the routines the program calls).
    if let Some((hp, he)) = heap_words {
        emit_heap_runtime(&mut asm, os.as_mut(), &ctx.heap_labels, hp, he, uses_msize);
    }

    os.wrap(asm, bss as u64)
}

/// Program-wide addressing / layout context shared by every [`FnEmit`].
struct Ctx {
    /// BSS byte offset of each IR global (by global id).
    global_bss: Vec<i32>,
    /// The `Fs` global's id (a `CTask *`), when the program uses exceptions.
    fs_gid: Option<u32>,
    /// BSS offset of the single `CTask` region (seeded into `Fs` at `@entry`).
    ctask_off: Option<i32>,
    /// `CTask` field offsets used by the unwinder.
    exc_top_off: i32,
    except_ch_off: i32,
    /// Heap-runtime entry labels (one per used routine).
    heap_labels: HashMap<&'static str, usize>,
    /// A ≥32-byte BSS scratch for the clock primitives' OS time structures.
    clock_scratch: Option<i32>,
    /// Command-line / environment BSS slots (the captured `argc`/`argv`/`envp` globals).
    argc_off: Option<i32>,
    argv_off: Option<i32>,
    envp_off: Option<i32>,
}

struct FnEmit<'a> {
    asm: &'a mut Asm,
    os: &'a mut dyn OsTarget,
    ctx: &'a Ctx,
    labels: &'a HashMap<&'a str, usize>,
    str_idx: &'a [usize],
    ret: IrRet,
    block_labels: Vec<usize>,
    /// Frame offset of each alloca slot (address = `rbp - off`).
    slot_off: Vec<i32>,
    /// Frame offset of each vreg's spill slot.
    vreg_off: Vec<i32>,
    /// Register-promotion plan: `vreg → Some(callee-saved GPR)` when promoted to
    /// rbx/r12–r14, else `None` (lives in its `vreg_off` slot). Empty for a
    /// `try`-containing function (promotion disabled — see `plan_registers`).
    vreg_reg: Vec<Option<PReg>>,
    /// Callee-saved registers this function promotes into, with the frame offset where
    /// the caller's value is saved across the body: `(reg, off)`.
    saved_regs: Vec<(u8, i32)>,
    /// Whether this is `@entry` (its `Ret` exits the process; it captures the command
    /// line and seeds `Fs`).
    is_entry: bool,
}

impl FnEmit<'_> {
    fn emit(&mut self, f: &IrFunc) -> Result<(), CodegenError> {
        // Plan register promotion: hot vregs → callee-saved rbx/r12–r14 (no float pool,
        // since System V has no callee-saved xmm). A spilled vreg (`None`) still gets a
        // frame slot below; a promoted one lives in its register, slot dead but harmless.
        // Linux only: the Windows PE's inlined kernel32 seam (`WriteFile`/`VirtualAlloc`/…)
        // does not reliably preserve a promoted value across a call, so the Windows target
        // keeps the proven spill-everything baseline (an empty pool = no promotion).
        let int_pool: &[u32] = if self.os.is_posix() {
            &PROMOTE_INT
        } else {
            &[]
        };
        self.vreg_reg = crate::regalloc::plan_registers(f, int_pool, &[]);

        // ---- frame layout (spill everything) ----
        let mut frame = 0i32;
        let mut alloc = |size: i32, align: i32| {
            let a = align.max(1);
            frame = (frame + size.max(1) + a - 1) / a * a;
            frame
        };
        self.slot_off = f
            .slots
            .iter()
            .map(|s| alloc(s.size as i32, s.align as i32))
            .collect();
        self.vreg_off = (0..f.n_vregs).map(|_| alloc(8, 8)).collect();

        // One save slot per distinct callee-saved register we promote into: this function
        // must preserve rbx/r12–r14 for its own caller, so it stashes the incoming value
        // in the prologue and restores it before returning.
        let mut used: Vec<u8> = Vec::new();
        for p in self.vreg_reg.iter().flatten() {
            let r = p.num as u8;
            if !used.contains(&r) {
                used.push(r);
            }
        }
        self.saved_regs = used.into_iter().map(|r| (r, alloc(8, 8))).collect();
        let frame_size = align16(frame);

        self.block_labels = f.blocks.iter().map(|_| self.asm.new_label()).collect();

        // ---- prologue ----
        self.asm.place(self.labels[f.name.as_str()]);
        let frame_pos = self.asm.prologue();

        // Save the caller's value of every callee-saved register we promote into. This
        // precedes the parameter stores below (which may overwrite a promoted register
        // with an incoming argument) and the `@entry` capture (which may clobber them,
        // before the body assigns each promoted vreg's def).
        for (reg, off) in self.saved_regs.clone() {
            self.asm.save_reg(reg, off);
        }

        if self.is_entry {
            // Capture the command line / environment into the global BSS slots.
            if let (Some(c), Some(v)) = (self.ctx.argc_off, self.ctx.argv_off) {
                self.os.emit_capture_args(self.asm, c, v);
            }
            if let Some(e) = self.ctx.envp_off {
                self.os.emit_capture_env(self.asm, e);
            }
            // Seed the single-task `Fs`: *(Fs global) = &CTask.
            if let (Some(fs), Some(ct)) = (self.ctx.fs_gid, self.ctx.ctask_off) {
                self.asm.lea_global(RAX, ct);
                self.asm.lea_global(RCX, self.ctx.global_bss[fs as usize]);
                self.asm.store_qword_at(RCX, RAX);
            }
        } else {
            // Store incoming parameters into their slots.
            let mut params = f.params.iter();
            if matches!(f.ret, IrRet::Agg { .. }) {
                if let Some(sret) = params.next() {
                    self.asm.mov_rr(RAX, R11); // sret pointer arrives in r11
                    self.store_vreg(sret.vreg, RAX);
                }
            }
            let mut igr = 0usize;
            let mut fpr = 0u8;
            for p in params {
                match p.ty {
                    ArgTy::Float => {
                        self.asm
                            .movsd_local_xmm(0x11, self.vreg_off[p.vreg as usize], fpr);
                        fpr += 1;
                    }
                    ArgTy::Int(_) | ArgTy::AggAddr { .. } => {
                        self.asm.mov_rax_argreg(igr);
                        self.store_vreg(p.vreg, RAX);
                        igr += 1;
                    }
                }
            }
        }

        // Walk the blocks via the shared driver.
        crate::backend::emit_blocks(self, f)?;

        self.asm.patch_frame(frame_pos, frame_size);
        Ok(())
    }

    // ---- value access (spill-all) ----

    /// Load an immediate into a GPR, picking the short form when it fits in i32.
    fn load_imm(&mut self, reg: u8, v: i64) {
        if let Ok(i) = i32::try_from(v) {
            self.asm.mov_ri(reg, i);
        } else {
            self.asm.mov_ri64(reg, v as u64);
        }
    }

    /// Load integer/pointer operand `v`'s 64 bits into low GPR `reg` (0–7). A promoted
    /// vreg lives in a callee-saved register, reached with a reg-reg `mov`. (Float vregs
    /// are never promoted, so only the int path needs this.)
    fn load_val(&mut self, v: Val, reg: u8) {
        match v {
            Val::Reg(r) => {
                if let Some(p) = self.vreg_reg[r as usize] {
                    self.asm.mov_rr(reg, p.num as u8);
                } else {
                    self.asm
                        .load_local_reg(reg, self.vreg_off[r as usize], 8, false);
                }
            }
            Val::ImmInt(i) => self.load_imm(reg, i),
            Val::ImmF64(b) => self.load_imm(reg, b as i64),
        }
    }

    /// Store low GPR `reg` (0–7) into vreg `dst` (its promoted register, or its slot).
    fn store_vreg(&mut self, dst: Vreg, reg: u8) {
        if let Some(p) = self.vreg_reg[dst as usize] {
            self.asm.mov_rr(p.num as u8, reg);
        } else {
            self.asm
                .store_local_reg(reg, self.vreg_off[dst as usize], 8);
        }
    }

    /// Load a float operand `v` into xmm `vr` (0–1).
    fn load_float(&mut self, v: Val, vr: u8) {
        match v {
            Val::Reg(r) => self
                .asm
                .movsd_local_xmm(0x10, self.vreg_off[r as usize], vr),
            Val::ImmF64(b) => {
                self.load_imm(RAX, b as i64);
                self.asm.movq_xmm_from_r(vr, RAX);
            }
            Val::ImmInt(i) => {
                self.load_imm(RAX, i);
                self.asm.movq_xmm_from_r(vr, RAX);
            }
        }
    }

    /// Store xmm `vr` (0–1) into vreg `dst`'s slot.
    fn store_float(&mut self, dst: Vreg, vr: u8) {
        self.asm
            .movsd_local_xmm(0x11, self.vreg_off[dst as usize], vr);
    }

    /// `reg = rbp - off + extra` (the address of an alloca slot plus a constant offset).
    fn slot_addr(&mut self, slot: SlotId, extra: u32, reg: u8) {
        self.asm.lea_local_reg(reg, self.slot_off[slot as usize]);
        if extra != 0 {
            self.asm.add_ri(reg, extra as i32);
        }
    }

    fn unsupported(&self, what: &str) -> CodegenError {
        CodegenError::new(format!("IR x86_64 backend: {what}"), None)
    }

    // ---- instruction selection ----

    fn sel_inst(&mut self, inst: &IrInst) -> Result<(), CodegenError> {
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
                    self.load_float(*lhs, 0);
                    self.load_float(*rhs, 1);
                    match op {
                        IrBinOp::Add => self.asm.addsd(0, 1),
                        IrBinOp::Sub => self.asm.subsd(0, 1),
                        IrBinOp::Mul => self.asm.mulsd(0, 1),
                        IrBinOp::Div => self.asm.divsd(0, 1),
                        IrBinOp::Mod => self.emit_fmod(),
                        _ => return Err(self.unsupported("bitwise op on a float")),
                    }
                    self.store_float(*dst, 0);
                } else {
                    self.load_val(*lhs, TMP0);
                    self.load_val(*rhs, TMP1);
                    self.emit_int_binop(*op, *signed)?;
                    self.store_vreg(*dst, TMP0);
                }
            }
            IrInst::Un { dst, op, ty, src } => {
                if ty.is_float() {
                    match op {
                        IrUnOp::Neg => {
                            self.load_float(*src, 0);
                            self.asm.movq_r_from_xmm(RAX, 0);
                            self.asm.btc_rax_63(); // flip the sign bit
                            self.asm.movq_xmm_from_r(0, RAX);
                            self.store_float(*dst, 0);
                        }
                        IrUnOp::BitNot => return Err(self.unsupported("bitwise not on a float")),
                    }
                } else {
                    self.load_val(*src, TMP0);
                    match op {
                        IrUnOp::Neg => self.asm.neg_rax(),
                        IrUnOp::BitNot => self.asm.not_rax(),
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
                self.emit_cmp_to_rax(*op, *ty, *signed, *lhs, *rhs);
                self.store_vreg(*dst, TMP0);
            }
            IrInst::Cast { dst, to, from, src } => match (from.is_float(), to.is_float()) {
                (false, false) => {
                    self.load_val(*src, TMP0);
                    self.asm.cast_rax(to.size() as i32, to.is_signed());
                    self.store_vreg(*dst, TMP0);
                }
                (true, true) => {
                    self.load_float(*src, 0);
                    self.store_float(*dst, 0);
                }
                (false, true) => {
                    self.load_val(*src, TMP0);
                    self.asm.cvtsi2sd(0, RAX); // signed int → double
                    self.store_float(*dst, 0);
                }
                (true, false) => {
                    self.load_float(*src, 0);
                    if matches!(to, IrTy::U8 | IrTy::U16 | IrTy::U32 | IrTy::U64) {
                        self.emit_f64_to_u64();
                    } else {
                        self.asm.cvttsd2si(RAX, 0);
                    }
                    self.asm.cast_rax(to.size() as i32, to.is_signed());
                    self.store_vreg(*dst, TMP0);
                }
            },
            IrInst::Mov { dst, src, ty } => {
                if ty.is_float() {
                    self.load_float(*src, 0);
                    self.store_float(*dst, 0);
                } else {
                    self.load_val(*src, TMP0);
                    self.store_vreg(*dst, TMP0);
                }
            }
            IrInst::SlotAddr { dst, slot, off } => {
                self.slot_addr(*slot, *off, TMP0);
                self.store_vreg(*dst, TMP0);
            }
            IrInst::GlobalAddr { dst, global, off } => {
                self.asm
                    .lea_global(RAX, self.ctx.global_bss[*global as usize]);
                if *off != 0 {
                    self.asm.add_ri(RAX, *off as i32);
                }
                self.store_vreg(*dst, TMP0);
            }
            IrInst::StrAddr { dst, str } => {
                self.asm.lea_rax_string(self.str_idx[*str as usize]);
                self.store_vreg(*dst, TMP0);
            }
            IrInst::FuncAddr { dst, func } => {
                let label = *self
                    .labels
                    .get(func.as_str())
                    .ok_or_else(|| self.unsupported("address of unlowered function"))?;
                self.asm.lea_rax_label(label);
                self.store_vreg(*dst, TMP0);
            }
            IrInst::PtrAdd {
                dst,
                base,
                index,
                stride,
            } => {
                self.load_val(*base, TMP1); // rcx = base
                self.load_val(*index, TMP0); // rax = index
                if *stride != 1 {
                    self.asm.imul_rax_imm32(*stride as i32);
                }
                self.asm.add_rax_rcx(); // rax = base + index*stride
                self.store_vreg(*dst, TMP0);
            }
            IrInst::Load { dst, ty, addr } => {
                self.load_val(*addr, TMP0);
                if ty.is_float() {
                    self.asm.movsd_load_at(RAX); // xmm0 = [rax]
                    self.store_float(*dst, 0);
                } else {
                    self.asm.load_through(ty.size() as i32, ty.is_signed());
                    self.store_vreg(*dst, TMP0);
                }
            }
            IrInst::Store { ty, addr, val } => {
                self.load_val(*addr, TMP1); // rcx = address (store_through writes [rcx])
                if ty.is_float() {
                    self.load_float(*val, 0);
                    self.asm.movsd_store_at(RCX);
                } else {
                    self.load_val(*val, TMP0);
                    self.asm.store_through(ty.size() as i32);
                }
            }
            IrInst::MemCpy { dst, src, len } => {
                self.load_val(*dst, ADDR2); // rdi = dst
                self.load_val(*src, ADDR); // rsi = src
                self.asm.mov_ri(RCX, *len as i32);
                self.asm.rep_movsb();
            }
            IrInst::MemZero { dst, len } => {
                self.load_val(*dst, ADDR2); // rdi = dst
                self.asm.xor_rr(RAX, RAX); // al = 0
                self.asm.mov_ri(RCX, *len as i32);
                self.asm.rep_stosb();
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
        }
        Ok(())
    }

    /// `rax = lhs <op> rhs` for integers (operands already in rax/rcx).
    fn emit_int_binop(&mut self, op: IrBinOp, signed: bool) -> Result<(), CodegenError> {
        match op {
            IrBinOp::Add => self.asm.add_rax_rcx(),
            IrBinOp::Sub => self.asm.sub_rax_rcx(),
            IrBinOp::Mul => self.asm.imul_rax_rcx(),
            IrBinOp::Div => {
                if signed {
                    self.asm.cqo();
                    self.asm.idiv_rcx();
                } else {
                    self.asm.div_rcx();
                }
            }
            IrBinOp::Mod => {
                if signed {
                    self.asm.cqo();
                    self.asm.idiv_rcx();
                } else {
                    self.asm.div_rcx();
                }
                self.asm.mov_rax_rdx(); // remainder is in rdx
            }
            IrBinOp::BitAnd => self.asm.and_rax_rcx(),
            IrBinOp::BitOr => self.asm.or_rax_rcx(),
            IrBinOp::BitXor => self.asm.xor_rax_rcx(),
            IrBinOp::Shl => self.asm.shl_rax_cl(),
            IrBinOp::Shr => {
                if signed {
                    self.asm.sar_rax_cl();
                } else {
                    self.asm.shr_rax_cl();
                }
            }
        }
        Ok(())
    }

    /// Materialize `lhs <op> rhs` as a 0/1 in rax.
    fn emit_cmp_to_rax(&mut self, op: CmpOp, ty: IrTy, signed: bool, lhs: Val, rhs: Val) {
        if ty.is_float() {
            self.load_float(lhs, 0);
            self.load_float(rhs, 1);
            self.asm.ucomisd(0, 1);
            self.asm.setcc_movzx(float_setcc(op));
        } else {
            self.load_val(lhs, TMP0);
            self.load_val(rhs, TMP1);
            self.asm.cmp_set(int_setcc(op, signed)); // cmp rax,rcx; setcc al; movzx
        }
    }

    /// `xmm0 = xmm0 % xmm1` (fmod): `a - trunc(a/b)*b`, with the truncation through the
    /// signed int round-trip (`cvttsd2si`/`cvtsi2sd`) — exact for the usual quotients,
    /// matching the freestanding arm64 backend.
    fn emit_fmod(&mut self) {
        // xmm2 = a, keep a copy of b in xmm1.
        self.asm.movsd_rr(2, 0); // xmm2 = a
        self.asm.divsd(0, 1); // xmm0 = a/b
        self.asm.cvttsd2si(RAX, 0); // rax = trunc(a/b)
        self.asm.cvtsi2sd(0, RAX); // xmm0 = (double)trunc(a/b)
        self.asm.mulsd(0, 1); // xmm0 = trunc(a/b)*b
        self.asm.subsd(2, 0); // xmm2 = a - trunc(a/b)*b
        self.asm.movsd_rr(0, 2); // result in xmm0
    }

    /// Convert xmm0 to a u64 in rax (Rust's saturating `f as u64`): negatives/NaN → 0,
    /// values ≥ 2^63 via a bias split.
    fn emit_f64_to_u64(&mut self) {
        let zero = self.asm.new_label();
        let big = self.asm.new_label();
        let done = self.asm.new_label();
        self.asm.mov_rax_imm(0);
        self.asm.movq_xmm_from_r(1, RAX); // xmm1 = 0.0
        self.asm.ucomisd(0, 1);
        self.asm.jb(zero); // value < 0 (NaN also sets CF)
        self.asm.jp(zero); // unordered (NaN)
        self.asm.mov_rax_imm(0x43E0_0000_0000_0000u64 as i64); // 2^63 as f64 bits
        self.asm.movq_xmm_from_r(1, RAX);
        self.asm.ucomisd(0, 1);
        self.asm.jae(big);
        self.asm.cvttsd2si(RAX, 0);
        self.asm.jmp(done);
        self.asm.place(big);
        self.asm.subsd(0, 1); // value - 2^63
        self.asm.cvttsd2si(RAX, 0);
        self.asm.btc_rax_63(); // + 2^63
        self.asm.jmp(done);
        self.asm.place(zero);
        self.asm.mov_rax_imm(0);
        self.asm.place(done);
    }

    // ---- calls ----

    fn emit_call(
        &mut self,
        dst: Option<Vreg>,
        ret: IrRet,
        callee: &Callee,
        args: &[ArgVal],
        sret: Option<Val>,
    ) -> Result<(), CodegenError> {
        // An optimization intrinsic (`Sqrt`/`Fabs`) with the `F64 -> F64` shape lowers to
        // a single SSE instruction instead of a call to its lib body.
        if let Callee::Direct(name) = callee {
            if self.try_intrinsic(name, ret, args, dst) {
                return Ok(());
            }
        }
        self.place_args(args, sret);
        match callee {
            Callee::Direct(name) => {
                let label = *self
                    .labels
                    .get(name.as_str())
                    .ok_or_else(|| self.unsupported("call to unlowered function"))?;
                self.asm.call(label);
            }
            Callee::Indirect(v) => {
                self.load_val(*v, RAX); // target in rax (args already placed)
                self.asm.call_reg(RAX);
            }
        }
        self.deliver_result(dst, ret);
        Ok(())
    }

    /// Place call arguments in the ABI registers (int/ptr in rdi/rsi/rdx/rcx/r8/r9, F64 in
    /// xmm0–7, the two classes numbered independently; sret pointer in r11). Each arg is
    /// read from its stable slot, so a forward placement order never clobbers a pending
    /// source.
    fn place_args(&mut self, args: &[ArgVal], sret: Option<Val>) {
        let mut igr = 0usize;
        let mut fpr = 0u8;
        for a in args {
            match a.ty {
                ArgTy::Float => {
                    self.load_float(a.val, fpr);
                    fpr += 1;
                }
                ArgTy::Int(_) | ArgTy::AggAddr { .. } => {
                    self.load_val(a.val, RAX);
                    self.asm.mov_rr(ARG_GPR[igr], RAX);
                    igr += 1;
                }
            }
        }
        if let Some(s) = sret {
            self.load_val(s, RAX);
            self.asm.mov_rr(R11, RAX); // sret pointer in r11
        }
    }

    fn deliver_result(&mut self, dst: Option<Vreg>, ret: IrRet) {
        if let Some(d) = dst {
            match ret {
                IrRet::Scalar(t) if t.is_float() => self.store_float(d, 0), // xmm0
                _ => self.store_vreg(d, 0),                                 // rax
            }
        }
    }

    /// Lower `Sqrt`/`Fabs` (the `F64 -> F64` optimization intrinsics) to a single SSE
    /// instruction in place of a call. Returns whether it was handled.
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
        match name {
            "Sqrt" => {
                self.load_float(arg.val, 0);
                self.asm.sqrtsd(0, 0);
            }
            "Fabs" => {
                // andpd with ~sign-bit clears bit 63.
                self.load_float(arg.val, 0);
                self.asm.mov_rax_imm(0x7FFF_FFFF_FFFF_FFFF);
                self.asm.movq_xmm_from_r(1, RAX);
                self.asm.andpd(0, 1);
            }
            // The rounding family needs SSE4.1 `roundsd`; keep the HolyC body on x86.
            _ => return false,
        }
        self.store_float(d, 0);
        true
    }

    // ---- primitives ----

    fn emit_prim(
        &mut self,
        dst: Option<Vreg>,
        prim: Prim,
        args: &[Val],
        width: Option<IrTy>,
    ) -> Result<(), CodegenError> {
        match prim {
            Prim::Free => {} // a no-op bump allocator never frees
            Prim::MAlloc => return self.emit_heap_call(dst, "MAlloc", args),
            Prim::HeapExtend => return self.emit_heap_call(dst, "HeapExtend", args),
            Prim::MSize => return self.emit_heap_call(dst, "MSize", args),
            Prim::StdWrite => {
                self.load_int_args(args);
                self.os.emit_std_write(self.asm);
                self.store_dst(dst);
            }
            Prim::Open => {
                self.load_int_args(args);
                self.os.emit_fileop(self.asm, FileOp::Open);
                self.store_dst(dst);
            }
            Prim::Read | Prim::Write | Prim::Close | Prim::LSeek => {
                let op = match prim {
                    Prim::Read => FileOp::Read,
                    Prim::Write => FileOp::Write,
                    Prim::Close => FileOp::Close,
                    _ => FileOp::LSeek,
                };
                self.load_int_args(args);
                self.os.emit_fileop(self.asm, op);
                self.store_dst(dst);
            }
            Prim::UnixNS | Prim::NanoNS | Prim::CpuNS => {
                let scratch = self.ctx.clock_scratch.expect("clock scratch");
                match prim {
                    Prim::NanoNS => self.os.emit_mono_ns(self.asm, scratch),
                    Prim::CpuNS => self.os.emit_cpu_ns(self.asm, scratch),
                    _ => self.os.emit_unix_ns(self.asm, scratch),
                }
                self.store_dst(dst);
            }
            Prim::Sleep => {
                self.load_val(args[0], RAX); // rax = ns
                let scratch = self.ctx.clock_scratch.expect("clock scratch");
                self.os.emit_sleep(self.asm, scratch);
            }
            Prim::Exit => {
                self.load_val(args[0], RAX);
                self.os.emit_exit(self.asm);
            }
            Prim::Socket
            | Prim::Connect
            | Prim::Remove
            | Prim::Rename
            | Prim::Mkdir
            | Prim::Getpid
            | Prim::Getppid
            | Prim::Getuid
            | Prim::Getgid
            | Prim::Chdir => {
                self.require_posix(prim)?;
                let nr: i32 = match prim {
                    Prim::Getpid => 39,
                    Prim::Chdir => 80,
                    Prim::Rename => 82,
                    Prim::Mkdir => 83,
                    Prim::Remove => 87,
                    Prim::Getuid => 102,
                    Prim::Getgid => 104,
                    Prim::Getppid => 110,
                    Prim::Socket => 41,
                    Prim::Connect => 42,
                    _ => unreachable!(),
                };
                self.load_int_args(args);
                self.asm.mov_ri(RAX, nr);
                self.asm.syscall();
                self.store_dst(dst);
            }
            Prim::Getcwd => {
                self.require_posix(prim)?;
                self.load_int_args(args); // rdi = buf, rsi = size
                self.asm.mov_ri(RAX, 79);
                self.asm.syscall();
                let neg = self.asm.new_label();
                self.asm.cmp_ri(RAX, 0);
                self.asm.js(neg);
                self.asm.xor_rr(RAX, RAX); // a length → 0
                self.asm.place(neg);
                self.store_dst(dst);
            }
            Prim::Thread => {
                self.require_posix(prim)?;
                self.emit_thread_fs(dst, args);
            }
            Prim::Join => {
                self.require_posix(prim)?;
                self.emit_join_fs(dst, args);
            }
            Prim::AtomicLoad
            | Prim::AtomicStore
            | Prim::AtomicAdd
            | Prim::AtomicSwap
            | Prim::AtomicCas => {
                self.emit_atomic(dst, prim, args, width.unwrap_or(IrTy::I64));
            }
            Prim::AtomicFence => self.asm.mfence(),
            Prim::FutexWait | Prim::FutexWake => {
                self.require_posix(prim)?;
                self.emit_futex(prim, args);
            }
        }
        Ok(())
    }

    fn require_posix(&self, prim: Prim) -> Result<(), CodegenError> {
        if self.os.is_posix() {
            Ok(())
        } else {
            Err(self.unsupported(&format!(
                "`{prim:?}` is not supported on the Windows target yet"
            )))
        }
    }

    /// Load up to three primitive arguments into the syscall/ABI registers rdi/rsi/rdx
    /// (all low registers, so the operands' slots load directly).
    fn load_int_args(&mut self, args: &[Val]) {
        for (i, &a) in args.iter().enumerate().take(3) {
            self.load_val(a, ARG_GPR[i]);
        }
    }

    fn store_dst(&mut self, dst: Option<Vreg>) {
        if let Some(d) = dst {
            self.store_vreg(d, RAX);
        }
    }

    /// Call a heap runtime routine (`MAlloc`/`HeapExtend`/`MSize`) with its args in
    /// rdi/rsi/rdx; the result is in rax.
    fn emit_heap_call(
        &mut self,
        dst: Option<Vreg>,
        routine: &str,
        args: &[Val],
    ) -> Result<(), CodegenError> {
        self.load_int_args(args);
        let label = *self
            .ctx
            .heap_labels
            .get(routine)
            .ok_or_else(|| self.unsupported("heap routine not emitted"))?;
        self.asm.call(label);
        self.store_dst(dst);
        Ok(())
    }

    /// An atomic op (`stdatomic.hc`), width-directed by `width` (the pointee type). On
    /// x86-64 a plain aligned `mov` is an atomic acquire load / release store; add/swap/cas
    /// use the `lock`-prefixed `xadd`/`xchg`/`cmpxchg`. The result is sign/zero-extended.
    fn emit_atomic(&mut self, dst: Option<Vreg>, prim: Prim, args: &[Val], width: IrTy) {
        let w = width.size() as i32;
        let signed = width.is_signed();
        match prim {
            Prim::AtomicLoad => {
                self.load_val(args[0], RAX); // rax = p
                self.asm.load_through(w, signed); // rax = [p]
            }
            Prim::AtomicStore => {
                self.load_val(args[0], RCX); // rcx = p (store_through writes [rcx])
                self.load_val(args[1], RAX); // rax = v
                self.asm.store_through(w);
            }
            Prim::AtomicAdd => {
                self.load_val(args[0], RSI); // rsi = p
                self.load_val(args[1], RAX); // rax = delta
                self.asm.mov_rr(RDX, RAX); // rdx = delta (kept past the xadd)
                self.asm.lock_xadd(RSI, RAX, w); // rax = old, [rsi] += delta
                self.asm.cast_rax(w, signed); // extend old
                self.asm.add_rr(RAX, RDX); // new = old + delta
                self.asm.cast_rax(w, signed);
            }
            Prim::AtomicSwap => {
                self.load_val(args[0], RSI); // rsi = p
                self.load_val(args[1], RAX); // rax = v
                self.asm.xchg_mem(RSI, RAX, w); // rax = old, [rsi] = v
                self.asm.cast_rax(w, signed);
            }
            Prim::AtomicCas => {
                self.load_val(args[0], RSI); // rsi = p
                self.load_val(args[1], RAX); // rax = expected (the cmpxchg comparand)
                self.load_val(args[2], RCX); // rcx = desired
                self.asm.lock_cmpxchg(RSI, RCX, w); // if [rsi]==acc then [rsi]=rcx; acc=old
                self.asm.cast_rax(w, signed);
            }
            _ => unreachable!(),
        }
        self.store_dst(dst);
    }

    /// `FutexWait(addr, val)` / `FutexWake(addr, n)` via the Linux `futex(2)` syscall
    /// (`FUTEX_WAIT` 0 / `FUTEX_WAKE` 1) on the low 32 bits of `*addr`. A `FutexWait`
    /// carries a short relative timeout, so a missed wakeup re-checks rather than deadlocks.
    fn emit_futex(&mut self, prim: Prim, args: &[Val]) {
        const FUTEX_TIMEOUT_NS: i32 = 1_000_000; // ≈1 ms
        let wake = matches!(prim, Prim::FutexWake);
        self.load_val(args[0], RDI); // rdi = uaddr
        self.load_val(args[1], RDX); // rdx = val (expected / n)
        self.asm.mov_ri(RSI, if wake { 1 } else { 0 }); // FUTEX_WAKE / FUTEX_WAIT
        if wake {
            self.asm.mov_ri(R10, 0); // no timeout
        } else {
            // Relative `struct timespec {0, FUTEX_TIMEOUT_NS}` on the stack -> r10.
            self.asm.add_ri(RSP, -16);
            self.asm.store_rsp_imm(0, 0); // tv_sec
            self.asm.store_rsp_imm(8, FUTEX_TIMEOUT_NS); // tv_nsec
            self.asm.mov_rr(R10, RSP);
        }
        self.asm.mov_ri(R8, 0); // uaddr2
        self.asm.mov_ri(R9, 0); // val3
        self.asm.mov_ri(RAX, 202); // SYS_futex
        self.asm.syscall();
        if !wake {
            self.asm.add_ri(RSP, 16);
        }
    }

    /// Freestanding `Thread(fn, arg)`: spawn a `CLONE_THREAD` thread via `clone(2)` onto an
    /// `mmap`'d stack running `fn(arg)`. A 32-byte control block at the stack base —
    /// `[retval | ctid futex | fn | arg]` — carries the closure in and the result back; its
    /// address is the handle. `base` rides into the child in callee-saved rbx (saved/restored
    /// around the spawn on the parent path).
    fn emit_thread_fs(&mut self, dst: Option<Vreg>, args: &[Val]) {
        const SIZE: i32 = 0x2_0000; // 128 KiB stack + control block
        // CLONE_VM|FS|FILES|SIGHAND|THREAD|SYSVSEM|SETTLS|PARENT_SETTID|CHILD_CLEARTID.
        const FLAGS: i32 = 0x3D_0F00;
        const TLS_OFF: i32 = 0x40; // a TLS self-pointer slot past the 32-byte block

        self.asm.push_reg(RBX); // save the caller's rbx
        // mmap(0, SIZE, PROT_READ|WRITE, MAP_PRIVATE|ANON, -1, 0) -> rax = base.
        self.asm.mov_ri(RDI, 0);
        self.asm.mov_ri(RSI, SIZE);
        self.asm.mov_ri(RDX, 3);
        self.asm.mov_ri(R10, 0x22);
        self.asm.mov_ri(R8, -1);
        self.asm.mov_ri(R9, 0);
        self.asm.mov_ri(RAX, 9); // mmap
        self.asm.syscall();
        self.asm.mov_rr(RBX, RAX); // rbx = base (survives the syscall, inherited by child)
        // control block: [base+16] = fn, [base+24] = arg.
        self.load_val(args[0], RAX);
        self.asm.mov_rr(RCX, RBX);
        self.asm.add_ri(RCX, 16);
        self.asm.store_qword_at(RCX, RAX);
        self.load_val(args[1], RAX);
        self.asm.mov_rr(RCX, RBX);
        self.asm.add_ri(RCX, 24);
        self.asm.store_qword_at(RCX, RAX);
        // TLS self-pointer: [base+TLS_OFF] = base+TLS_OFF.
        self.asm.mov_rr(RCX, RBX);
        self.asm.add_ri(RCX, TLS_OFF);
        self.asm.store_qword_at(RCX, RCX);

        let l_child = self.asm.new_label();
        let l_done = self.asm.new_label();
        // clone(FLAGS, child_sp, ptid=&futex, ctid=&futex, tls=&TLS).
        self.asm.mov_rr(RSI, RBX);
        self.asm.add_ri(RSI, SIZE - 16); // child stack top
        self.asm.mov_rr(RDX, RBX);
        self.asm.add_ri(RDX, 8); // ptid = &futex
        self.asm.mov_rr(R10, RBX);
        self.asm.add_ri(R10, 8); // ctid = &futex
        self.asm.mov_rr(R8, RBX);
        self.asm.add_ri(R8, TLS_OFF); // tls = &TLS
        self.asm.mov_ri(RDI, FLAGS);
        self.asm.mov_ri(RAX, 56); // clone
        self.asm.syscall();
        self.asm.test_rax();
        self.asm.je(l_child);
        // Parent: rbx still holds base (the handle). Restore rbx and finish.
        self.asm.mov_rr(RAX, RBX);
        self.asm.pop_reg(RBX);
        self.asm.jmp(l_done);
        // Child: rax == 0, rbx = base. Run fn(arg), stash the result, exit.
        self.asm.place(l_child);
        self.asm.mov_rr(RDX, RBX);
        self.asm.add_ri(RDX, 24);
        self.asm.load_qword_at(RDI, RDX); // rdi = arg
        self.asm.mov_rr(RDX, RBX);
        self.asm.add_ri(RDX, 16);
        self.asm.load_qword_at(RAX, RDX); // rax = fn
        self.asm.call_reg(RAX); // fn(arg); rbx survives (callee-saved)
        self.asm.store_qword_at(RBX, RAX); // [base+0] = return
        self.asm.mov_ri(RDI, 0);
        self.asm.mov_ri(RAX, 60); // exit (this thread; fires CLONE_CHILD_CLEARTID)
        self.asm.syscall();
        self.asm.place(l_done);
        self.store_dst(dst);
    }

    /// Freestanding `Join(handle)`: futex-wait on the control block's `ctid` word until the
    /// kernel clears it (thread exit), then return the stashed `retval`. `base` is held in
    /// callee-saved rbx across the syscall (saved/restored).
    fn emit_join_fs(&mut self, dst: Option<Vreg>, args: &[Val]) {
        self.asm.push_reg(RBX);
        self.load_val(args[0], RAX);
        self.asm.mov_rr(RBX, RAX); // rbx = base (handle)
        let l_wait = self.asm.new_label();
        let l_done = self.asm.new_label();
        self.asm.place(l_wait);
        self.asm.mov_rr(RCX, RBX);
        self.asm.add_ri(RCX, 8);
        self.asm.load_qword_at(RAX, RCX); // rax = *futex (0 once the thread exits)
        self.asm.test_rax();
        self.asm.je(l_done);
        self.asm.mov_rr(RDX, RAX); // val = observed tid
        self.asm.mov_rr(RDI, RBX);
        self.asm.add_ri(RDI, 8); // uaddr = &futex
        self.asm.mov_ri(RSI, 0); // FUTEX_WAIT
        self.asm.mov_ri(R10, 0); // timeout = NULL
        self.asm.mov_ri(RAX, 202); // SYS_futex
        self.asm.syscall();
        self.asm.jmp(l_wait);
        self.asm.place(l_done);
        self.asm.mov_rr(RAX, RBX);
        self.asm.load_qword_at(RAX, RAX); // rax = [base+0] = retval
        self.asm.pop_reg(RBX);
        self.store_dst(dst);
    }

    // ---- terminators ----

    fn sel_term(&mut self, term: &IrTerm) -> Result<(), CodegenError> {
        match term {
            IrTerm::Br(t) => self.asm.jmp(self.block_labels[*t as usize]),
            IrTerm::CondBr { cond, t, f } => {
                let (tl, fl) = (
                    self.block_labels[*t as usize],
                    self.block_labels[*f as usize],
                );
                match cond {
                    Cond::NonZero { val, ty } => {
                        if ty.is_float() {
                            self.load_float(*val, 0);
                            self.asm.mov_rax_imm(0);
                            self.asm.movq_xmm_from_r(1, RAX); // xmm1 = 0.0
                            self.asm.ucomisd(0, 1);
                            self.asm.jne(tl); // != 0.0
                            self.asm.jp(tl); // NaN is truthy
                        } else {
                            self.load_val(*val, TMP0);
                            self.asm.test_rax();
                            self.asm.jne(tl);
                        }
                    }
                    Cond::Cmp {
                        op,
                        ty,
                        signed,
                        lhs,
                        rhs,
                    } => {
                        self.emit_cmp_to_rax(*op, *ty, *signed, *lhs, *rhs);
                        self.asm.test_rax();
                        self.asm.jne(tl);
                    }
                }
                self.asm.jmp(fl);
            }
            IrTerm::Switch {
                val,
                cases,
                default,
                ..
            } => {
                self.load_val(*val, TMP0);
                for (lo, hi, blk) in cases {
                    let target = self.block_labels[*blk as usize];
                    if lo == hi {
                        self.cmp_rax_imm(*lo);
                        self.asm.je(target);
                    } else {
                        let skip = self.asm.new_label();
                        self.cmp_rax_imm(*lo);
                        self.asm.jl(skip);
                        self.cmp_rax_imm(*hi);
                        self.asm.jg(skip);
                        self.asm.jmp(target);
                        self.asm.place(skip);
                    }
                }
                self.asm.jmp(self.block_labels[*default as usize]);
            }
            IrTerm::Ret(v) => {
                match v {
                    Some(val) if matches!(self.ret, IrRet::Scalar(t) if t.is_float()) => {
                        self.load_float(*val, 0)
                    }
                    Some(val) => self.load_val(*val, RAX),
                    None => self.asm.mov_ri(RAX, 0),
                }
                self.teardown();
            }
            // The throw value and `Fs` flags were written by the lowering's `Store`s, so
            // both `throw expr;` and bare `throw;` reduce to the same unwind.
            IrTerm::Throw(_) | IrTerm::Rethrow => self.emit_unwind(),
            IrTerm::Unreachable => {
                self.asm.mov_ri(RAX, 0);
                self.teardown();
            }
        }
        Ok(())
    }

    /// Return from the function with the result already in rax/xmm0: `@entry` exits the
    /// process (no restore — `exit` does not return); any other function restores the
    /// callee-saved registers it promoted into, then tears down the frame. The restores
    /// touch only rbx/r12–r14, leaving the rax/xmm0 result intact.
    fn teardown(&mut self) {
        if self.is_entry {
            self.os.emit_exit(self.asm);
        } else {
            for (reg, off) in self.saved_regs.clone() {
                self.asm.restore_reg(reg, off);
            }
            self.asm.epilogue();
        }
    }

    /// `cmp rax, imm`: an i32 immediate compares directly; a wide one goes through rcx.
    fn cmp_rax_imm(&mut self, imm: i64) {
        if let Ok(i) = i32::try_from(imm) {
            self.asm.cmp_ri(RAX, i);
        } else {
            self.asm.mov_ri64(RCX, imm as u64);
            self.asm.cmp_rr(RAX, RCX);
        }
    }

    // ---- exceptions (jmp_buf/longjmp-style unwind over Fs->exc_top) ----
    //
    // The `ExcFrame` (32 bytes) is `{ prev, saved_rsp, saved_rbp, landing_pad }`. Spill-all
    // keeps nothing in callee-saved registers, so no callee-saved set is saved.

    /// Load the current task pointer (`*Fs`, a `CTask *`) into `reg`.
    fn fs_ptr(&mut self, reg: u8) {
        let fs = self.ctx.fs_gid.expect("Fs accessed without the Fs global");
        self.asm.lea_global(reg, self.ctx.global_bss[fs as usize]);
        self.asm.load_qword_at(reg, reg); // reg = *(&Fs) = CTask*
    }

    /// `TryBegin`: build the on-stack `ExcFrame` and push it onto `Fs->exc_top`. The frame
    /// is the alloca `frame`; its fields are at `[rbp - slot_off ..]`.
    fn emit_try_begin(&mut self, pad: BlockId, frame: SlotId) {
        let exc_top = self.ctx.exc_top_off;
        // frame.prev = Fs->exc_top
        self.fs_ptr(ADDR); // rsi = CTask*
        self.asm.mov_rr(RAX, ADDR);
        self.asm.add_ri(RAX, exc_top);
        self.asm.load_through(8, false); // rax = Fs->exc_top
        self.slot_addr(frame, 0, RCX); // rcx = &ExcFrame
        self.asm.store_qword_at(RCX, RAX); // frame.prev
        // frame.saved_rsp / saved_rbp.
        self.slot_addr(frame, 8, RCX);
        self.asm.store_qword_at(RCX, RSP);
        self.slot_addr(frame, 16, RCX);
        self.asm.store_qword_at(RCX, RBP);
        // frame.landing_pad = &pad
        self.asm.lea_rax_label(self.block_labels[pad as usize]);
        self.slot_addr(frame, 24, RCX);
        self.asm.store_qword_at(RCX, RAX);
        // Fs->exc_top = &ExcFrame
        self.fs_ptr(RCX);
        self.asm.add_ri(RCX, exc_top);
        self.slot_addr(frame, 0, RAX);
        self.asm.store_qword_at(RCX, RAX);
    }

    /// `TryEnd`: normal completion pops the handler (`Fs->exc_top = Fs->exc_top->prev`).
    fn emit_try_end(&mut self) {
        let exc_top = self.ctx.exc_top_off;
        self.fs_ptr(ADDR); // rsi = CTask*
        self.asm.mov_rr(RCX, ADDR);
        self.asm.add_ri(RCX, exc_top); // rcx = &exc_top
        self.asm.load_qword_at(RAX, RCX); // rax = top
        self.asm.load_qword_at(RAX, RAX); // rax = top->prev (offset 0)
        self.asm.store_qword_at(RCX, RAX);
    }

    /// `Throw`/`Rethrow`: unwind to the nearest handler — restore its rsp/rbp from the top
    /// `ExcFrame`, pop it, and jump to its landing pad. An empty chain exits with the thrown
    /// value (`Fs->except_ch`).
    fn emit_unwind(&mut self) {
        let exc_top = self.ctx.exc_top_off;
        self.fs_ptr(ADDR); // rsi = CTask*
        self.asm.mov_rr(RCX, ADDR);
        self.asm.add_ri(RCX, exc_top);
        self.asm.load_qword_at(R11, RCX); // r11 = top frame
        let live = self.asm.new_label();
        self.asm.test_rr(R11, R11);
        self.asm.jne(live);
        // Uncaught: exit(Fs->except_ch).
        self.asm.mov_rr(RAX, ADDR);
        self.asm.add_ri(RAX, self.ctx.except_ch_off);
        self.asm.load_through(8, false);
        self.os.emit_exit(self.asm);
        self.asm.place(live);
        // Fs->exc_top = top->prev.
        self.asm.load_qword_at(RAX, R11); // rax = top->prev
        self.fs_ptr(RCX);
        self.asm.add_ri(RCX, exc_top);
        self.asm.store_qword_at(RCX, RAX);
        // Restore rsp then rbp from the frame, then jump to its landing pad.
        self.asm.mov_rr(RAX, R11);
        self.asm.add_ri(RAX, 8);
        self.asm.load_through(8, false);
        self.asm.mov_rr(RSP, RAX); // saved_rsp
        self.asm.mov_rr(RAX, R11);
        self.asm.add_ri(RAX, 16);
        self.asm.load_through(8, false);
        self.asm.mov_rr(RBP, RAX); // saved_rbp
        self.asm.mov_rr(RAX, R11);
        self.asm.add_ri(RAX, 24);
        self.asm.load_through(8, false); // landing_pad
        self.asm.jmp_reg(RAX);
    }
}

// ---- heap runtime (bump allocator over OS page allocations) ----

/// Emit the heap routines the program calls, each at its label.
fn emit_heap_runtime(
    asm: &mut Asm,
    os: &mut dyn OsTarget,
    labels: &HashMap<&'static str, usize>,
    hp: i32,
    he: i32,
    uses_msize: bool,
) {
    if let Some(&l) = labels.get("MAlloc") {
        asm.place(l);
        emit_rt_malloc(asm, os, hp, he, uses_msize);
    }
    if let Some(&l) = labels.get("HeapExtend") {
        asm.place(l);
        emit_rt_heapextend(asm, hp, he, uses_msize);
    }
    if let Some(&l) = labels.get("MSize") {
        asm.place(l);
        emit_rt_msize(asm);
    }
    if let Some(&l) = labels.get("Free") {
        asm.place(l);
        asm.emit(&[0xC3]); // a no-op bump allocator never frees
    }
}

/// `MAlloc(rdi=n) -> rax`: bump allocator over OS page chunks (≥1 MiB, page-aligned).
fn emit_rt_malloc(asm: &mut Asm, os: &mut dyn OsTarget, hp: i32, he: i32, uses_msize: bool) {
    let alloc = asm.new_label();
    let sized = asm.new_label();
    asm.push_reg(RBX); // preserve rbx (survives the page-alloc call)
    if uses_msize {
        asm.push_reg(RDI); // keep the original n for the header
    }
    asm.add_ri(RDI, 15);
    asm.and_ri(RDI, -16);
    if uses_msize {
        asm.add_ri(RDI, 16); // reserve a 16-byte size header
    }
    asm.mov_rr(RBX, RDI); // rbx = total bytes to bump
    asm.lea_global(R9, hp);
    asm.load_qword_at(RAX, R9); // rax = *heap_ptr
    asm.lea_global(R10, he);
    asm.load_qword_at(R8, R10); // r8 = *heap_end
    asm.mov_rr(RCX, RAX);
    asm.add_rr(RCX, RBX); // rcx = ptr + n
    asm.cmp_reg_reg(RCX, R8);
    asm.jbe(alloc); // fits in the current chunk
    asm.mov_rr(RSI, RBX);
    asm.mov_ri(RCX, 0x10_0000);
    asm.cmp_reg_reg(RSI, RCX);
    asm.jae(sized);
    asm.mov_rr(RSI, RCX);
    asm.place(sized);
    asm.add_ri(RSI, 4095);
    asm.and_ri(RSI, -4096);
    os.emit_page_alloc(asm); // base -> rax, rsi kept
    asm.mov_rr(R8, RAX);
    asm.add_rr(R8, RSI); // r8 = base + chunk size
    asm.lea_global(R10, he);
    asm.store_qword_at(R10, R8); // *heap_end = base + size
    asm.place(alloc);
    asm.mov_rr(RCX, RAX);
    asm.add_rr(RCX, RBX);
    asm.lea_global(R9, hp);
    asm.store_qword_at(R9, RCX); // *heap_ptr = base + n
    if uses_msize {
        asm.pop_reg(RCX); // the original n
        asm.store_qword_at(RAX, RCX); // [base] = n (the size header)
        asm.add_ri(RAX, 16); // return base + 16
    }
    asm.pop_reg(RBX);
    asm.emit(&[0xC3]); // ret
}

/// `HeapExtend(rdi=ptr, rsi=old, rdx=new) -> rax`: grow the bump allocator's last block in
/// place when it still fits the chunk, else NULL.
fn emit_rt_heapextend(asm: &mut Asm, hp: i32, he: i32, uses_msize: bool) {
    let null = asm.new_label();
    asm.test_rr(RDI, RDI);
    asm.je(null);
    asm.mov_rr(RAX, RSI);
    asm.add_ri(RAX, 15);
    asm.and_ri(RAX, -16);
    asm.mov_rr(RCX, RDX);
    asm.add_ri(RCX, 15);
    asm.and_ri(RCX, -16);
    asm.mov_rr(R8, RDI);
    asm.add_rr(R8, RAX); // r8 = block end
    asm.lea_global(R9, hp);
    asm.load_qword_at(R10, R9); // r10 = *heap_ptr
    asm.cmp_reg_reg(R8, R10);
    asm.jne(null);
    asm.mov_rr(R8, RDI);
    asm.add_rr(R8, RCX); // r8 = ptr + align16(new)
    asm.lea_global(R11, he);
    asm.load_qword_at(RAX, R11); // rax = *heap_end
    asm.cmp_reg_reg(RAX, R8);
    asm.jb(null);
    asm.store_qword_at(R9, R8); // *heap_ptr = ptr + anew
    if uses_msize {
        asm.mov_rr(RCX, RDI);
        asm.add_ri(RCX, -16);
        asm.store_qword_at(RCX, RDX); // [ptr-16] = new size
    }
    asm.mov_rr(RAX, RDI);
    asm.emit(&[0xC3]);
    asm.place(null);
    asm.mov_ri(RAX, 0);
    asm.emit(&[0xC3]);
}

/// `MSize(rdi=ptr) -> rax`: the requested size in `ptr`'s header (`*(ptr-16)`), 0 for NULL.
fn emit_rt_msize(asm: &mut Asm) {
    let nz = asm.new_label();
    asm.test_rr(RDI, RDI);
    asm.jne(nz);
    asm.mov_ri(RAX, 0);
    asm.emit(&[0xC3]);
    asm.place(nz);
    asm.mov_rr(RAX, RDI);
    asm.add_ri(RAX, -16);
    asm.load_qword_at(RAX, RAX); // rax = *(ptr - 16)
    asm.emit(&[0xC3]);
}

/// The block-walk driver ([`crate::backend::emit_blocks`]) drives a `FnEmit` through these
/// per-arch leaf emits.
impl crate::backend::Backend for FnEmit<'_> {
    fn place_block(&mut self, i: usize) {
        self.asm.place(self.block_labels[i]);
    }
    fn emit_inst(&mut self, inst: &IrInst) -> Result<(), CodegenError> {
        self.sel_inst(inst)
    }
    fn emit_term(&mut self, term: &IrTerm) -> Result<(), CodegenError> {
        self.sel_term(term)
    }
}

/// `setcc` for an integer comparison, signedness-directed.
fn int_setcc(op: CmpOp, signed: bool) -> u8 {
    match (op, signed) {
        (CmpOp::Eq, _) => SETE,
        (CmpOp::Ne, _) => SETNE,
        (CmpOp::Lt, true) => SETL,
        (CmpOp::Gt, true) => SETG,
        (CmpOp::Le, true) => SETLE,
        (CmpOp::Ge, true) => SETGE,
        (CmpOp::Lt, false) => SETB,
        (CmpOp::Gt, false) => SETA,
        (CmpOp::Le, false) => SETBE,
        (CmpOp::Ge, false) => SETAE,
    }
}

/// `setcc` for an F64 comparison (`ucomisd` sets CF/ZF unsigned-style).
fn float_setcc(op: CmpOp) -> u8 {
    match op {
        CmpOp::Eq => SETE,
        CmpOp::Ne => SETNE,
        CmpOp::Lt => SETB,
        CmpOp::Gt => SETA,
        CmpOp::Le => SETBE,
        CmpOp::Ge => SETAE,
    }
}
