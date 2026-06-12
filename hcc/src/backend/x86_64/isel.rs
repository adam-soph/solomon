//! x86-64 instruction selection — the x86 backend's machine-code generator.
//!
//! This is **not** an IR; it *consumes* the one SSA [IR](crate::ir) (shared with the
//! interpreter and the arm64 backend) and emits x86-64. The block-walk driver and the
//! pure-IR analyses are shared via [`crate::backend`]; this module supplies the per-arch
//! instruction selection, ABI, exception unwind, primitives, and `Asm` encoding.
//!
//! It lowers a program to the SSA [IR](crate::ir), destructs it out of SSA
//! ([`crate::backend::destruct_program`]), and emits x86-64 by walking the resulting
//! `phi`-free blocks — reusing the [`Asm`](crate::backend::x86_64::asm::Asm) encoder, the freestanding-ELF
//! writer (Linux), and the kernel32-import PE writer (Windows), both behind the
//! [`OsTarget`](crate::backend::x86_64::OsTarget) seam. It replaces the old AST-walking codegen and the
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
//! `{prev, saved_rsp, saved_rbp, landing_pad}` (32 bytes, no saved callee-saved set — a
//! `try`-containing function is left fully spilled by `plan_registers`, so the unwinder has
//! nothing to restore). The algebraic intrinsics `Sqrt`/`Fabs` lower to `sqrtsd`/`andpd`.

use std::collections::{HashMap, HashSet};

use crate::backend::CodegenError;
use crate::backend::Location;
use crate::backend::x86_64::asm::Asm;
use crate::backend::x86_64::{
    FileOp, OsTarget, R8, R9, R10, R11, R12, R13, R14, R15, RAX, RBP, RBX, RCX, RDI, RDX, RSI, RSP,
    align16,
};
use crate::ir::*;

mod exc;
mod heap;
mod prims;
use heap::emit_heap_runtime;

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

/// Emit a runnable executable image from the phi-free, out-of-SSA `ir` (produced by
/// [`crate::backend::lower_to_machine_ir`]), with the OS-specific steps (exit, page alloc,
/// stdout, file ops, clock, container) deferred to `os`.
pub(super) fn compile_ir(ir: &Program, mut os: Box<dyn OsTarget>) -> Result<Vec<u8>, CodegenError> {
    // x86-64 has no guaranteed population-count instruction (`popcnt` is SSE4.2, not in the
    // freestanding baseline), so expand `UnOp::Popcount` to the portable SWAR sequence here;
    // the rest of the backend never sees the op.
    let owned = crate::backend::idiom::expand_popcount(ir);
    let ir = &owned;
    // Reachable functions from `@entry`, over direct calls and `&Func`, `@entry` first
    // (it is the program entry point — the first code byte).
    let reachable = crate::backend::reachable_functions(ir, "x86_64 backend")?;

    // Implicit-global ids, the `CTask` layout, and Fs-use — all resolved the same way as the
    // arm64 backend, so they live in `crate::backend`.
    let ig = crate::backend::implicit_globals(ir);
    let (fs_gid, argc_gid, argv_gid, envp_gid) = (ig.fs, ig.argc, ig.argv, ig.envp);
    let ctask = crate::backend::ctask_layout(ir)?;
    let (exc_top_off, except_ch_off) = (ctask.exc_top as i32, ctask.except_ch as i32);
    let ctask_size = ctask.size as i32;
    // `Fs` is registered unconditionally by lowering (to match the interpreter), but a
    // program that never touches it or throws needs neither the `CTask` region nor the
    // `Fs` pointer slot — gate both on real use so a non-exception program stays lean.
    let prog_uses_fs = crate::backend::prog_uses_fs(&reachable, fs_gid);
    // Exceptions disable register promotion program-wide (see `Ctx::program_has_exceptions`).
    let program_has_exceptions = crate::backend::program_has_exceptions(ir);

    // Which impure primitive groups the reachable code uses, to size BSS and emit only the
    // heap routines actually needed. (Heap set is shared; the clock scratch is x86-only.)
    let heap_used = crate::backend::heap_prims_used(&reachable);
    let uses_clock = reachable.iter().flat_map(|f| &f.blocks).any(|b| {
        b.insts.iter().any(|i| {
            matches!(
                i,
                Inst::Prim {
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
    let need_args = argc_gid.is_some() || argv_gid.is_some();
    let argc_off = need_args.then(|| {
        argc_gid
            .map(|g| global_bss[g as usize])
            .unwrap_or_else(|| alloc_bss(8, 8))
    });
    let argv_off = need_args.then(|| {
        argv_gid
            .map(|g| global_bss[g as usize])
            .unwrap_or_else(|| alloc_bss(8, 8))
    });
    let envp_off = envp_gid.map(|g| global_bss[g as usize]);
    let ctx = Ctx {
        global_bss,
        program_has_exceptions,
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
            fusable: HashSet::new(),
            pending_fuse: HashMap::new(),
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
    /// `true` if the program uses exceptions (`try`/`throw`) anywhere. Disables register
    /// promotion program-wide (an unwinding `throw` would not restore a promoted callee-saved
    /// register in an abandoned intermediate frame — see `backend::plan_registers`).
    program_has_exceptions: bool,
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
    ret: Ret,
    block_labels: Vec<usize>,
    /// Frame offset of each alloca slot (address = `rbp - off`).
    slot_off: Vec<i32>,
    /// Frame offset of each vreg's spill slot.
    vreg_off: Vec<i32>,
    /// Where each vreg lives: a physical register (`Location::Reg`) or its `vreg_off` slot
    /// (`Location::Spill`). From `backend::allocate`; all-`Spill` when the program uses
    /// exceptions (allocation disabled program-wide).
    vreg_reg: Vec<Location>,
    /// Callee-saved registers this function promotes into, with the frame offset where
    /// the caller's value is saved across the body: `(reg, off)`.
    saved_regs: Vec<(u8, i32)>,
    /// Whether this is `@entry` (its `Ret` exits the process; it captures the command
    /// line and seeds `Fs`).
    is_entry: bool,
    /// Addressing-mode fusion (T3): `PtrAdd` dst vregs used exactly once by an immediately-
    /// following `Load`/`Store` with a SIB-legal stride (1/2/4/8). Such a `PtrAdd` defers (no
    /// standalone emit); the indexing folds into the memory op's `[base + index*scale]`.
    /// Adjacency keeps `base`/`index` evaluated where the `PtrAdd` would have, so the
    /// already-computed register liveness stays valid.
    fusable: HashSet<Vreg>,
    /// A deferred fusable `PtrAdd`'s `(base, index, stride)`, keyed by dst.
    pending_fuse: HashMap<Vreg, (Val, Val, u32)>,
}

/// The `PtrAdd` dst vregs eligible for addressing-mode fusion (T3): used exactly once, by an
/// *immediately-following* `Load`/`Store` in the same block, with a SIB-legal stride
/// (1/2/4/8). The adjacent single use keeps `base`/`index` evaluated at the `PtrAdd`'s program
/// point, so the already-computed register liveness stays valid.
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
            let consumes = match &w[1] {
                Inst::Load {
                    addr: Val::Reg(v), ..
                }
                | Inst::Store {
                    addr: Val::Reg(v), ..
                } => *v == dst,
                _ => false,
            };
            // SIB scale must be 1, 2, 4, or 8.
            if consumes && matches!(stride, 1 | 2 | 4 | 8) {
                fusable.insert(dst);
            }
        }
    }
    fusable
}

/// If `c` is a power of two `2^k` with `k` in `1..=62`, return `k`. Mirrors `simplify::pow2`'s
/// bound, so the signed div/mod strength reduction covers exactly the cases `simplify` leaves
/// for the backend (unsigned by `2^k` is already a shift/mask).
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
        // Callee-saved pool rbx/r12–r14 + the free caller-saved GPRs r10/r8/r9 (used only for
        // ranges that don't cross a call). r8/r9 are argument registers, so `place_args`
        // defers writing them until every arg's source has been read (see its pass 2); rax/rcx/
        // rdx + rsi/rdi (rep-movs scratch) and r11 (sret) stay reserved. No float pool yet —
        // `load_float`/`store_float` don't consult the allocation until a later phase adds xmm
        // promotion. Safe on Windows now that the prologue stack-probes large frames (see
        // `Asm::prologue_probe`).
        let regset = crate::backend::RegSet {
            callee_int: PROMOTE_INT.to_vec(),
            caller_int: vec![R10 as u32, R8 as u32, R9 as u32],
            callee_flt: Vec::new(),
            caller_flt: Vec::new(),
        };
        let regs = crate::backend::allocate(f, &regset, self.ctx.program_has_exceptions);
        self.vreg_reg = regs.loc;
        self.fusable = compute_fusable(f);

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

        // One save slot per callee-saved register the allocator used: this function must
        // preserve rbx/r12–r14 for its own caller, so it stashes the incoming value in the
        // prologue and restores it before returning. (Caller-saved promotions, when Phase 4
        // adds them, are not in `regs.saved` and need no slot.)
        self.saved_regs = regs
            .saved
            .iter()
            .map(|p| (p.num as u8, alloc(8, 8)))
            .collect();
        let frame_size = align16(frame);

        self.block_labels = f.blocks.iter().map(|_| self.asm.new_label()).collect();

        // ---- prologue ----
        self.asm.place(self.labels[f.name.as_str()]);
        // Windows commits only one stack page, and we emit no `__chkstk`, so a frame larger
        // than a page must probe each page as it descends or a deep access faults (Linux
        // grows the stack on demand). The probe bakes in the frame size; the plain prologue
        // patches it later.
        let frame_pos = if !self.os.is_posix() && frame_size > 4096 {
            self.asm.prologue_probe(frame_size);
            None
        } else {
            Some(self.asm.prologue())
        };

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
            if matches!(f.ret, Ret::Agg { .. }) {
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
                        if igr < ARG_GPR.len() {
                            self.asm.mov_rax_argreg(igr);
                        } else {
                            // Stack-passed (7th+ int arg): the caller placed it above the
                            // saved rbp + return address, at `[rbp + 16 + (igr-6)*8]`.
                            // `load_local_reg` reads `[rbp - off]`, so a negative off is a
                            // positive displacement.
                            let stk = (igr - ARG_GPR.len()) as i32;
                            self.asm.load_local_reg(RAX, -(16 + stk * 8), 8, false);
                        }
                        self.store_vreg(p.vreg, RAX);
                        igr += 1;
                    }
                }
            }
        }

        // Walk the blocks via the shared driver.
        crate::backend::emit_blocks(self, f)?;

        // The probed prologue baked the frame size in; the plain one needs a patch.
        if let Some(pos) = frame_pos {
            self.asm.patch_frame(pos, frame_size);
        }
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
            Val::Reg(r) => match self.vreg_reg[r as usize] {
                Location::Reg(p) => self.asm.mov_rr(reg, p.num as u8),
                Location::Spill => {
                    self.asm
                        .load_local_reg(reg, self.vreg_off[r as usize], 8, false);
                }
            },
            Val::ImmInt(i) => self.load_imm(reg, i),
            Val::ImmF64(b) => self.load_imm(reg, b as i64),
        }
    }

    /// Store low GPR `reg` (0–7) into vreg `dst` (its promoted register, or its slot).
    fn store_vreg(&mut self, dst: Vreg, reg: u8) {
        match self.vreg_reg[dst as usize] {
            Location::Reg(p) => self.asm.mov_rr(p.num as u8, reg),
            Location::Spill => {
                self.asm
                    .store_local_reg(reg, self.vreg_off[dst as usize], 8);
            }
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
                    self.load_float(*lhs, 0);
                    self.load_float(*rhs, 1);
                    match op {
                        BinOp::Add => self.asm.addsd(0, 1),
                        BinOp::Sub => self.asm.subsd(0, 1),
                        BinOp::Mul => self.asm.mulsd(0, 1),
                        BinOp::Div => self.asm.divsd(0, 1),
                        BinOp::Mod => self.emit_fmod(),
                        _ => return Err(self.unsupported("bitwise op on a float")),
                    }
                    self.store_float(*dst, 0);
                } else if !self.try_imm_div(*dst, *op, *signed, *lhs, *rhs) {
                    self.load_val(*lhs, TMP0);
                    // Immediate-form selection: `TMP0 op= #imm` directly when the constant
                    // fits, instead of materializing it into the rcx scratch first.
                    let imm_done = match *rhs {
                        Val::ImmInt(i) => self.try_imm_binop(*op, *signed, i),
                        _ => false,
                    };
                    if !imm_done {
                        self.load_val(*rhs, TMP1);
                        self.emit_int_binop(*op, *signed)?;
                    }
                    self.store_vreg(*dst, TMP0);
                }
            }
            Inst::Un { dst, op, ty, src } => {
                if ty.is_float() {
                    match op {
                        UnOp::Neg => {
                            self.load_float(*src, 0);
                            self.asm.movq_r_from_xmm(RAX, 0);
                            self.asm.btc_rax_63(); // flip the sign bit
                            self.asm.movq_xmm_from_r(0, RAX);
                            self.store_float(*dst, 0);
                        }
                        UnOp::BitNot => return Err(self.unsupported("bitwise not on a float")),
                        // Expanded to SWAR Bins before isel (see `compile_ir`).
                        UnOp::Popcount => unreachable!("popcount expanded for x86"),
                    }
                } else {
                    self.load_val(*src, TMP0);
                    match op {
                        UnOp::Neg => self.asm.neg_rax(),
                        UnOp::BitNot => self.asm.not_rax(),
                        UnOp::Popcount => unreachable!("popcount expanded for x86"),
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
                self.emit_cmp_to_rax(*op, *ty, *signed, *lhs, *rhs);
                self.store_vreg(*dst, TMP0);
            }
            Inst::Cast { dst, to, from, src } => match (from.is_float(), to.is_float()) {
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
                    if matches!(to, Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64) {
                        self.emit_f64_to_u64();
                    } else {
                        self.asm.cvttsd2si(RAX, 0);
                    }
                    self.asm.cast_rax(to.size() as i32, to.is_signed());
                    self.store_vreg(*dst, TMP0);
                }
            },
            Inst::Mov { dst, src, ty } => {
                if ty.is_float() {
                    self.load_float(*src, 0);
                    self.store_float(*dst, 0);
                } else {
                    self.load_val(*src, TMP0);
                    self.store_vreg(*dst, TMP0);
                }
            }
            Inst::SlotAddr { dst, slot, off } => {
                self.slot_addr(*slot, *off, TMP0);
                self.store_vreg(*dst, TMP0);
            }
            Inst::GlobalAddr { dst, global, off } => {
                self.asm
                    .lea_global(RAX, self.ctx.global_bss[*global as usize]);
                if *off != 0 {
                    self.asm.add_ri(RAX, *off as i32);
                }
                self.store_vreg(*dst, TMP0);
            }
            Inst::StrAddr { dst, str } => {
                self.asm.lea_rax_string(self.str_idx[*str as usize]);
                self.store_vreg(*dst, TMP0);
            }
            Inst::FuncAddr { dst, func } => {
                let label = *self
                    .labels
                    .get(func.as_str())
                    .ok_or_else(|| self.unsupported("address of unlowered function"))?;
                self.asm.lea_rax_label(label);
                self.store_vreg(*dst, TMP0);
            }
            Inst::PtrAdd {
                dst,
                base,
                index,
                stride,
            } => {
                // Fusable into the next mem op: defer (emit nothing); the indexing folds into
                // the load/store's SIB `[base + index*scale]`.
                if self.fusable.contains(dst) {
                    self.pending_fuse.insert(*dst, (*base, *index, *stride));
                } else {
                    self.load_val(*base, TMP1); // rcx = base
                    self.load_val(*index, TMP0); // rax = index
                    if *stride != 1 {
                        self.asm.imul_rax_imm32(*stride as i32);
                    }
                    self.asm.add_rax_rcx(); // rax = base + index*stride
                    self.store_vreg(*dst, TMP0);
                }
            }
            Inst::Load { dst, ty, addr } => {
                if let Some((base, index, stride)) =
                    addr.reg().and_then(|v| self.pending_fuse.remove(&v))
                {
                    // base→rcx, index→rsi read here (where the deferred PtrAdd read them),
                    // before the result reg is written.
                    self.load_val(base, RCX);
                    self.load_val(index, RSI);
                    let scale = stride.trailing_zeros() as u8;
                    if ty.is_float() {
                        self.asm.movsd_load_sib(scale);
                        self.store_float(*dst, 0);
                    } else {
                        self.asm.load_sib(ty.size() as i32, ty.is_signed(), scale);
                        self.store_vreg(*dst, TMP0);
                    }
                } else {
                    self.load_val(*addr, TMP0);
                    if ty.is_float() {
                        self.asm.movsd_load_at(RAX); // xmm0 = [rax]
                        self.store_float(*dst, 0);
                    } else {
                        self.asm.load_through(ty.size() as i32, ty.is_signed());
                        self.store_vreg(*dst, TMP0);
                    }
                }
            }
            Inst::Store { ty, addr, val } => {
                if let Some((base, index, stride)) =
                    addr.reg().and_then(|v| self.pending_fuse.remove(&v))
                {
                    self.load_val(base, RCX);
                    self.load_val(index, RSI);
                    let scale = stride.trailing_zeros() as u8;
                    if ty.is_float() {
                        self.load_float(*val, 0);
                        self.asm.movsd_store_sib(scale);
                    } else {
                        self.load_val(*val, TMP0);
                        self.asm.store_sib(ty.size() as i32, scale);
                    }
                } else if ty.is_float() {
                    self.load_val(*addr, TMP1); // rcx = address (store_through writes [rcx])
                    self.load_float(*val, 0);
                    self.asm.movsd_store_at(RCX);
                } else {
                    self.load_val(*addr, TMP1); // rcx = address (store_through writes [rcx])
                    self.load_val(*val, TMP0);
                    self.asm.store_through(ty.size() as i32);
                }
            }
            Inst::MemCpy { dst, src, len } => {
                self.load_val(*dst, ADDR2); // rdi = dst
                self.load_val(*src, ADDR); // rsi = src
                self.asm.mov_ri(RCX, *len as i32);
                self.asm.rep_movsb();
            }
            Inst::MemZero { dst, len } => {
                self.load_val(*dst, ADDR2); // rdi = dst
                self.asm.xor_rr(RAX, RAX); // al = 0
                self.asm.mov_ri(RCX, *len as i32);
                self.asm.rep_stosb();
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
        }
        Ok(())
    }

    /// Emit `TMP0 = TMP0 <op> #imm` with an immediate-form instruction when `imm` is
    /// encodable, avoiding the materialize-into-rcx the register form needs. Returns false
    /// (the caller then loads `imm` and uses `emit_int_binop`) when there is no usable form.
    /// The 64-bit operation matches `emit_int_binop`; ALU ops take a sign-extended imm32, so
    /// the constant must fit `i32` (then its sign-extension equals the i64 `imm`). Mul and
    /// divide/modulo keep the register path (T1 already reduced power-of-2 cases to shifts).
    fn try_imm_binop(&mut self, op: BinOp, signed: bool, imm: i64) -> bool {
        let fits32 = i32::try_from(imm).is_ok();
        let i = imm as i32;
        match op {
            BinOp::Add if fits32 => self.asm.add_ri(TMP0, i),
            BinOp::Sub if fits32 => self.asm.sub_ri(TMP0, i),
            BinOp::BitAnd if fits32 => self.asm.and_ri(TMP0, i),
            BinOp::BitOr if fits32 => self.asm.or_ri(TMP0, i),
            BinOp::BitXor if fits32 => self.asm.xor_ri(TMP0, i),
            BinOp::Shl if (0..64).contains(&imm) => self.asm.shl_ri(TMP0, imm as u8),
            BinOp::Shr if !signed && (0..64).contains(&imm) => self.asm.shr_ri(TMP0, imm as u8),
            BinOp::Shr if signed && (0..64).contains(&imm) => self.asm.sar_ri(TMP0, imm as u8),
            _ => return false,
        }
        true
    }

    /// Signed divide/modulo by a power of two `2^k`, strength-reduced to a branchless
    /// round-toward-zero sequence instead of the `cqo; idiv` (with its `-1` overflow guard).
    /// Returns false (the caller falls back to the generic path) unless `op` is signed
    /// `Div`/`Mod` and `rhs` is a constant power of two. (Unsigned div/mod by `2^k` is already
    /// reduced to a shift/mask in `simplify`, so it never reaches here as `Div`/`Mod`.)
    ///
    /// Mirrors the arm64 sequence: `bias = (x >>s 63) >>u (64 - k)` (`2^k-1` if `x<0` else 0),
    /// `q = (x + bias) >>s k` truncates toward zero, bit-identical to `idiv`. `Mod` then forms
    /// the remainder `r = x - (q << k)`. Scratch: rcx (free here — `rhs` is the immediate) and,
    /// for `Mod`, rdx (to hold `x` across the quotient computation); both are caller-saved.
    fn try_imm_div(&mut self, dst: Vreg, op: BinOp, signed: bool, lhs: Val, rhs: Val) -> bool {
        if !signed || !matches!(op, BinOp::Div | BinOp::Mod) {
            return false;
        }
        let Val::ImmInt(c) = rhs else { return false };
        let Some(k) = pow2(c) else { return false };
        let k = k as u8;
        self.load_val(lhs, TMP0); // rax = x
        self.asm.mov_rr(TMP1, TMP0); // rcx = x
        self.asm.sar_ri(TMP1, 63); // sign mask: all-ones if x<0 else 0
        self.asm.shr_ri(TMP1, 64 - k); // bias = (2^k - 1) if x<0 else 0
        if matches!(op, BinOp::Div) {
            self.asm.add_rax_rcx(); // x + bias
            self.asm.sar_ri(TMP0, k); // q = (x + bias) >>s k
        } else {
            self.asm.mov_rr(RDX, TMP0); // rdx = x (saved for the final subtract)
            self.asm.add_rax_rcx(); // rax = x + bias
            self.asm.sar_ri(TMP0, k); // q
            self.asm.shl_ri(TMP0, k); // q << k
            self.asm.mov_rr(TMP1, TMP0); // rcx = q << k
            self.asm.mov_rr(TMP0, RDX); // rax = x
            self.asm.sub_rax_rcx(); // r = x - (q << k)
        }
        self.store_vreg(dst, TMP0);
        true
    }

    /// `rax = lhs <op> rhs` for integers (operands already in rax/rcx).
    fn emit_int_binop(&mut self, op: BinOp, signed: bool) -> Result<(), CodegenError> {
        match op {
            BinOp::Add => self.asm.add_rax_rcx(),
            BinOp::Sub => self.asm.sub_rax_rcx(),
            BinOp::Mul => self.asm.imul_rax_rcx(),
            BinOp::Div => {
                if signed {
                    self.emit_signed_div(false);
                } else {
                    self.asm.div_rcx();
                }
            }
            BinOp::Mod => {
                if signed {
                    self.emit_signed_div(true);
                } else {
                    self.asm.div_rcx();
                    self.asm.mov_rax_rdx(); // remainder is in rdx
                }
            }
            BinOp::BitAnd => self.asm.and_rax_rcx(),
            BinOp::BitOr => self.asm.or_rax_rcx(),
            BinOp::BitXor => self.asm.xor_rax_rcx(),
            BinOp::Shl => self.asm.shl_rax_cl(),
            BinOp::Shr => {
                if signed {
                    self.asm.sar_rax_cl();
                } else {
                    self.asm.shr_rax_cl();
                }
            }
        }
        Ok(())
    }

    /// Signed divide/modulo of rax by rcx, result in rax. `idiv` raises `#DE` (a SIGFPE
    /// crash) on `i64::MIN / -1` — the quotient `2^63` overflows i64 — whereas the
    /// interpreter and arm64 `sdiv` give the wrapping result. So guard divisor == -1: then
    /// `a / -1` is `-a` (`neg`, which wraps `i64::MIN` to itself) and `a % -1` is 0, correct
    /// for every dividend and trap-free. The common (divisor != -1) path is the normal
    /// `cqo; idiv`, one predicted-not-taken compare heavier.
    fn emit_signed_div(&mut self, is_mod: bool) {
        let neg_one = self.asm.new_label();
        let done = self.asm.new_label();
        self.asm.cmp_ri(RCX, -1);
        self.asm.je(neg_one);
        self.asm.cqo();
        self.asm.idiv_rcx();
        if is_mod {
            self.asm.mov_rax_rdx(); // remainder is in rdx
        }
        self.asm.jmp(done);
        self.asm.place(neg_one);
        if is_mod {
            self.asm.mov_rax_imm(0); // a % -1 == 0
        } else {
            self.asm.neg_rax(); // a / -1 == -a (wraps for i64::MIN)
        }
        self.asm.place(done);
    }

    /// Materialize `lhs <op> rhs` as a 0/1 in rax.
    ///
    /// Float comparisons must match the IEEE/oracle result on NaN: every relation is false
    /// when an operand is NaN, except `!=` which is true. `ucomisd` reports "unordered" as
    /// `ZF=PF=CF=1`, so the CF-based `SETA`/`SETAE` are already NaN-safe (false on unordered)
    /// but `SETB`/`SETBE` are not — so `<`/`<=` are emitted as the swapped `>`/`>=` (`a<b`
    /// ≡ `b>a`), and `==`/`!=` fold in the parity flag (see `float_eq_movzx`/`float_ne_movzx`).
    fn emit_cmp_to_rax(&mut self, op: CmpOp, ty: Ty, signed: bool, lhs: Val, rhs: Val) {
        if ty.is_float() {
            match op {
                // `a<b` ≡ `b>a`, `a<=b` ≡ `b>=a`: swap operands so the NaN-safe SETA/SETAE apply.
                CmpOp::Lt | CmpOp::Le => {
                    self.load_float(rhs, 0);
                    self.load_float(lhs, 1);
                    self.asm.ucomisd(0, 1);
                    self.asm
                        .setcc_movzx(if matches!(op, CmpOp::Lt) { SETA } else { SETAE });
                }
                // `>`/`>=`: SETA/SETAE are false on unordered, so no swap needed.
                CmpOp::Gt | CmpOp::Ge => {
                    self.load_float(lhs, 0);
                    self.load_float(rhs, 1);
                    self.asm.ucomisd(0, 1);
                    self.asm
                        .setcc_movzx(if matches!(op, CmpOp::Gt) { SETA } else { SETAE });
                }
                // `==`/`!=`: account for the unordered (NaN) parity flag.
                CmpOp::Eq | CmpOp::Ne => {
                    self.load_float(lhs, 0);
                    self.load_float(rhs, 1);
                    self.asm.ucomisd(0, 1);
                    if matches!(op, CmpOp::Eq) {
                        self.asm.float_eq_movzx();
                    } else {
                        self.asm.float_ne_movzx();
                    }
                }
            }
        } else {
            self.load_val(lhs, TMP0);
            self.load_val(rhs, TMP1);
            self.asm.cmp_set(int_setcc(op, signed)); // cmp rax,rcx; setcc al; movzx
        }
    }

    /// `xmm0 = xmm0 % xmm1` (fmod): `a - trunc(a/b)*b`, with the truncation through the
    /// signed int round-trip (`cvttsd2si`/`cvtsi2sd`) — exact for the usual quotients,
    /// matching the freestanding arm64 backend.
    /// Float `%`: the truncated remainder `a - trunc(a/b)*b` (HolyC's `Fmod` form). The
    /// quotient is truncated toward zero with `cvttsd2si`, but that saturates to i64::MIN
    /// (x86's "integer indefinite") when `|a/b| >= 2^63` or the quotient is NaN. In that
    /// case the quotient is already integral (or NaN), so the round-trip is skipped and it
    /// is used as-is — matching arm64 `frintz` and the interpreter's `f64::trunc` exactly,
    /// including for huge quotients where the old unconditional round-trip diverged.
    fn emit_fmod(&mut self) {
        self.asm.movsd_rr(2, 0); // xmm2 = a
        self.asm.divsd(0, 1); // xmm0 = a/b = q
        self.asm.cvttsd2si(RAX, 0); // rax = trunc(q), or i64::MIN on |q|>=2^63 / NaN
        let integral = self.asm.new_label();
        self.asm.mov_ri64(RCX, i64::MIN as u64); // the saturation sentinel
        self.asm.cmp_rax_rcx();
        self.asm.je(integral); // q already integral (or NaN): keep xmm0 = q
        self.asm.cvtsi2sd(0, RAX); // xmm0 = (f64)trunc(q)
        self.asm.place(integral);
        self.asm.mulsd(0, 1); // xmm0 = trunc(q)*b
        self.asm.subsd(2, 0); // xmm2 = a - trunc(q)*b
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
        ret: Ret,
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
        let stack_bytes = self.place_args(args, sret);
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
        // Release any stack-argument block (caller-cleaned), restoring rsp before reading the
        // result.
        if stack_bytes > 0 {
            self.asm.add_ri(RSP, stack_bytes);
        }
        self.deliver_result(dst, ret);
        Ok(())
    }

    /// Place call arguments in the ABI registers (int/ptr in rdi/rsi/rdx/rcx/r8/r9, F64 in
    /// xmm0–7, the two classes numbered independently; sret pointer in r11). Each arg is
    /// read from its stable slot, so a forward placement order never clobbers a pending
    /// source.
    /// Place call arguments and return the bytes reserved on the stack for arguments beyond
    /// the registers (the caller restores rsp after the call). Integer/pointer args past the
    /// six `ARG_GPR` registers are passed on the stack (the internal ABI's 7th–8th int args,
    /// matching arm64's eight x-registers): reserve a 16-aligned block, place each overflow
    /// arg at `[rsp + k*8]`. rsp is 16-aligned at the call site and the reservation is a
    /// multiple of 16, so alignment is preserved through the `call`'s return-address push.
    fn place_args(&mut self, args: &[ArgVal], sret: Option<Val>) -> i32 {
        let n_int = args
            .iter()
            .filter(|a| matches!(a.ty, ArgTy::Int(_) | ArgTy::AggAddr { .. }))
            .count();
        let stack_bytes = align16((n_int.saturating_sub(ARG_GPR.len()) as i32) * 8);
        if stack_bytes > 0 {
            self.asm.sub_ri(RSP, stack_bytes);
        }
        // r8/r9 (`ARG_GPR[4..6]`) double as caller-saved promotion registers, so an arg whose
        // *source* is a vreg promoted into r8/r9 must be read before any arg *writes* r8/r9.
        // The other targets (rdi/rsi/rdx/rcx, the stack, and the rax/r11 scratch) are never
        // promotion registers, so they can never clobber a pending source. Pass 1 places those
        // (each through rax); pass 2 reads the at-most-two r8/r9-target sources into rax/r11
        // *first*, then writes r8/r9 — correct even when they swap (arg5←r9, arg6←r8).
        let mut igr = 0usize;
        let mut fpr = 0u8;
        let mut stk = 0i32;
        let mut deferred: Vec<(u8, Val)> = Vec::new(); // (target r8/r9, source)
        for a in args {
            match a.ty {
                ArgTy::Float => {
                    self.load_float(a.val, fpr);
                    fpr += 1;
                }
                ArgTy::Int(_) | ArgTy::AggAddr { .. } => {
                    if igr < ARG_GPR.len() {
                        let target = ARG_GPR[igr];
                        if target == R8 || target == R9 {
                            deferred.push((target, a.val));
                        } else {
                            self.load_val(a.val, RAX);
                            self.asm.mov_rr(target, RAX);
                        }
                    } else {
                        self.load_val(a.val, RAX);
                        self.asm.store_qword_rsp(stk * 8);
                        stk += 1;
                    }
                    igr += 1;
                }
            }
        }
        // Pass 2: r8/r9 targets. Read all sources into scratch (rax, r11) before writing.
        let scratch = [RAX, R11];
        for (i, (_, src)) in deferred.iter().enumerate() {
            self.load_val(*src, scratch[i]);
        }
        for (i, (target, _)) in deferred.iter().enumerate() {
            self.asm.mov_rr(*target, scratch[i]);
        }
        if let Some(s) = sret {
            self.load_val(s, RAX);
            self.asm.mov_rr(R11, RAX); // sret pointer in r11
        }
        stack_bytes
    }

    fn deliver_result(&mut self, dst: Option<Vreg>, ret: Ret) {
        if let Some(d) = dst {
            match ret {
                Ret::Scalar(t) if t.is_float() => self.store_float(d, 0), // xmm0
                _ => self.store_vreg(d, 0),                               // rax
            }
        }
    }

    /// Lower `Sqrt`/`Fabs` (the `F64 -> F64` optimization intrinsics) to a single SSE
    /// instruction in place of a call. Returns whether it was handled.
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

    // ---- terminators ----

    fn sel_term(&mut self, term: &Term) -> Result<(), CodegenError> {
        match term {
            Term::Br(t) => self.asm.jmp(self.block_labels[*t as usize]),
            Term::CondBr { cond, t, f } => {
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
            Term::Switch {
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
            Term::Ret(v) => {
                match v {
                    Some(val) if matches!(self.ret, Ret::Scalar(t) if t.is_float()) => {
                        self.load_float(*val, 0)
                    }
                    Some(val) => self.load_val(*val, RAX),
                    None => self.asm.mov_ri(RAX, 0),
                }
                self.teardown();
            }
            // The throw value and `Fs` flags were written by the lowering's `Store`s, so
            // both `throw expr;` and bare `throw;` reduce to the same unwind.
            Term::Throw(_) | Term::Rethrow => self.emit_unwind(),
            Term::Unreachable => {
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
