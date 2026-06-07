//! Code-generation backend for Apple-silicon macOS (`aarch64-apple-darwin`).
//!
//! It lowers the program to hand-emitted AArch64 machine code, writes a Mach-O
//! relocatable object, and links it with the system `cc`. There is no
//! LLVM/Cranelift/C: both the instruction bytes and the object container are
//! produced here. This backend matches the [interpreter](crate::interp)
//! byte-for-byte; the interpreter is the conformance oracle (see `tests/arm64_darwin.rs`).
//!
//! ## Scope
//!
//! Codegen is type-directed: it reads the typed AST (`Expr::ty`) and the
//! [layout pass](crate::layout) for field offsets, element strides, and access
//! widths. It compiles the whole implemented HolyC subset:
//!
//!   * functions/calls/recursion (incl. default and variadic args), all control
//!     flow, and `switch` (a dense one lowers to an O(1) jump table);
//!   * integer arithmetic with C width/signedness rules, `F64` (in the FP
//!     register file), and `Print`/string formatting via libc;
//!   * `&x`, `*p`, `p->f`, `s.f`, `a[i]`, pointer arithmetic (pointee-scaled),
//!     pointer comparison/difference; width-aware loads/stores with sign/zero
//!     extension; `sizeof`/`offset`, integer casts;
//!   * classes/unions by value (sret returns), arrays (decay to pointers),
//!     brace/designated aggregate initializers, function pointers (`ADR`+`BLR`);
//!   * global variables (Mach-O common symbols addressed via
//!     `PAGE21`/`PAGEOFF12` relocations) and the built-in library (lowered to
//!     libc externs via `BRANCH26` relocations).
//!
//! Several optimizations run during/after emission: constant folding,
//! immediate-form arithmetic, a dead-`mov` peephole, and a linear-scan register
//! allocator that promotes hot locals to callee-saved registers (see
//! `plan_registers` / `Asm::peephole`).
//!
//! The prologue is `stp x29,x30,[sp,#-16]!; mov x29,sp; sub sp,sp,#frame`. Locals
//! live below the frame pointer, addressed as `x29 - offset`. Promoted callee-saved
//! registers are spilled into the same frame with `stur`/`fstur` and restored in the
//! epilogue. The epilogue `mov sp,x29; ldp x29,x30,[sp],#16; ret` needs no frame
//! size, so only the one `sub sp` immediate is back-patched. Expression evaluation
//! is a stack machine: intermediates spill to the machine stack, so values survive
//! calls.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::ast::*;
use crate::codegen::{Codegen, CodegenError};
use crate::layout::Layouts;
use crate::token::{Pos, Span};

mod asm;
mod darwin;
mod linux;

pub use linux::Arm64Linux;

use asm::{Asm, CodeImage};

/// Safety-net timeout for `FutexWait`, about 1 ms. A missed wakeup degrades to a
/// re-check at this period instead of deadlocking. Never reached when wakeups work.
const FUTEX_TIMEOUT_NS: i64 = 1_000_000;

const RES: u32 = 9; // integer/pointer expression result
const T2: u32 = 10; // secondary integer temporary
const SCRATCH: u32 = 8; // scratch (e.g. `%` quotient, strides, fp<->gpr conduit)
const FRES: u32 = 16; // F64 expression result (v16, caller-saved)
const FT2: u32 = 17; // secondary F64 temporary (v17)
const FP: u32 = 29;
const LR: u32 = 30;
const SP: u32 = 31;

const XZR: u32 = 31;

const COND_EQ: u32 = 0b0000;
const COND_NE: u32 = 0b0001;
const COND_HS: u32 = 0b0010; // unsigned higher-or-same (>=)
const COND_LO: u32 = 0b0011; // unsigned lower (<)
const COND_HI: u32 = 0b1000; // unsigned higher (>) — also table bounds
const COND_LS: u32 = 0b1001; // unsigned lower-or-same (<=)
const COND_GE: u32 = 0b1010;
const COND_LT: u32 = 0b1011;
const COND_GT: u32 = 0b1100;
const COND_LE: u32 = 0b1101;

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

    /// Whether variadic arguments (to `printf`/`sprintf`/…) are passed in registers
    /// or all on the stack. `true` for standard AAPCS64; `false` for Apple's ARM64
    /// ABI. This is the only codegen difference between the two AArch64 OSes.
    fn variadic_in_registers(&self) -> bool;

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
        compile(program, &darwin::Darwin)
    }
}

/// Emit a relocatable object for `program` using `target`'s container format.
/// This driver handles function and global symbol layout, code emission, and fixup
/// resolution. It is shared by every AArch64 target; only `target` differs.
fn compile(program: &Program, target: &dyn ArmTarget) -> Result<Vec<u8>, CodegenError> {
    let (layouts, _) = crate::layout::compute(program);
    let mut cg = Cg::new(layouts);
    cg.variadic_regs = target.variadic_in_registers();
    cg.freestanding = target.freestanding();

    let main_label = cg.asm.new_label();
    let start_label = cg.asm.new_label();
    for item in &program.items {
        if let StmtKind::Func(f) = &item.kind {
            if f.body.is_some() {
                let label = cg.asm.new_label();
                cg.funcs.insert(
                    f.name.clone(),
                    FnInfo {
                        label,
                        params: f.params.clone(),
                        ret: f.ret.clone(),
                        varargs: f.varargs,
                    },
                );
            }
        }
    }

    // Defined symbols are `_main` + functions, in order. Globals follow them
    // in the symbol table, so a global's symbol index is `ndefined + ordinal`.
    let ndefined = 1 + cg.funcs.len() as u32;
    for item in &program.items {
        if let StmtKind::VarDecl { decls } = &item.kind {
            for d in decls {
                let sym = ndefined + cg.global_order.len() as u32;
                cg.globals.insert(
                    d.name.clone(),
                    GlobalInfo {
                        sym,
                        ty: d.ty.clone(),
                    },
                );
                cg.global_order.push(d.name.clone());
            }
        }
    }
    // `MSize` makes `MAlloc` prepend an 8-byte size header. Gate it so that
    // size-agnostic programs keep the lean, header-free heap byte-for-byte.
    cg.uses_msize = crate::ast::program_calls_any(program, &["MSize"]);
    // The command line is exposed as the implicit globals `ArgC`/`ArgV`: `_main`
    // captures x0/x1 into the two hidden symbols below. (A `...` function's `VargC`/
    // `VargV` varargs locals are distinct names.) Done only when the program
    // references them, so arg-free programs stay byte-for-byte unchanged.
    if crate::ast::program_uses_ident(program, &["ArgC", "ArgV"]) {
        cg.uses_args = true;
        for name in [ARGC_GLOBAL, ARGV_GLOBAL] {
            let sym = ndefined + cg.global_order.len() as u32;
            cg.globals
                .insert(name.to_string(), GlobalInfo { sym, ty: Type::U64 });
            cg.global_order.push(name.to_string());
        }
    }
    // The environment is the implicit global `U8 **EnvP`: `_main` captures the
    // `main` 3rd arg (x2) into the hidden symbol below. Gated independently of the
    // command line, so an `EnvP`-free program is byte-for-byte unchanged.
    if crate::ast::program_uses_ident(program, &["EnvP"]) {
        cg.uses_env = true;
        let sym = ndefined + cg.global_order.len() as u32;
        cg.globals
            .insert(ENVP_GLOBAL.to_string(), GlobalInfo { sym, ty: Type::U64 });
        cg.global_order.push(ENVP_GLOBAL.to_string());
    }
    // The current task `CTask *Fs`, backing exception state (`Fs->except_ch`). It is a
    // single hidden, zero-initialized `CTask` region; `gen_fs_ptr` loads its address.
    // (Process-global for now; the per-thread TLS upgrade is localized there.)
    if crate::ast::program_uses_ident(program, &["Fs"])
        || crate::ast::program_has_exceptions(program)
    {
        cg.uses_exc = true;
        // The freestanding process-global `CTask` region (single-task fallback).
        let sym = ndefined + cg.global_order.len() as u32;
        cg.globals.insert(
            CTASK_GLOBAL.to_string(),
            GlobalInfo {
                sym,
                ty: Type::Named("CTask".to_string()),
            },
        );
        cg.global_order.push(CTASK_GLOBAL.to_string());
        cg.ctask_sym = Some(sym);
        // The hosted (Darwin) per-thread TLS key (`pthread_key_t`). Harmless on
        // freestanding, which never references it.
        let ksym = ndefined + cg.global_order.len() as u32;
        cg.globals.insert(
            FSKEY_GLOBAL.to_string(),
            GlobalInfo {
                sym: ksym,
                ty: Type::U64,
            },
        );
        cg.global_order.push(FSKEY_GLOBAL.to_string());
        cg.fs_key_sym = Some(ksym);
    }

    // Freestanding: there is no linker to allocate commons, so lay the globals out
    // in BSS in declaration order with natural alignment. `addr_global` then
    // addresses each by a fixed offset.
    if cg.freestanding {
        for name in cg.global_order.clone() {
            let g = &cg.globals[&name];
            let (size, align) = (
                cg.layouts.size_of(&g.ty).max(1) as u64,
                cg.layouts.align_of(&g.ty).max(1) as u64,
            );
            let sym = g.sym;
            let off = cg.alloc_bss_fs(size, align);
            cg.global_bss.insert(sym, off);
        }
    }

    // Freestanding: emit `_start`, the ELF entry at the first byte of `__text`. It
    // calls `_main`, then `exit_group` with its return value. Hosted targets instead
    // let the libc start-up code call `_main` and turn its return into the exit
    // status.
    if cg.freestanding {
        cg.asm.place(start_label);
        // Per-thread exception state: point the main thread's user thread pointer
        // (`TPIDR_EL0`) at the global `CTask`. Spawned threads set their own in the
        // `clone` child. Gated on use, so non-exception programs are unchanged.
        if cg.uses_exc {
            let sym = cg.ctask_sym.expect("CTask region");
            cg.addr_global(SCRATCH, sym);
            cg.asm.msr_tpidr_el0(SCRATCH);
        }
        // At the ELF entry the kernel leaves the initial stack as
        // `[sp]=argc, [sp+8..]=argv[0..argc], NULL, envp[..], NULL, …`. `_main`
        // captures argc/argv/envp from x0/x1/x2 (the Darwin libc convention), so the
        // freestanding `_start` must materialise them in those registers from the
        // stack before the call. Without this `_main` reads stale registers and argc
        // comes out 0. Gated on use, so an arg-free program's entry is unchanged.
        if cg.uses_args || cg.uses_env {
            cg.asm.load_mem(0, SP, 8, false); // x0 = [sp] = argc
        }
        if cg.uses_args {
            cg.asm.add_imm(1, SP, 8); // x1 = sp + 8 = &argv[0]
        }
        if cg.uses_env {
            // x2 = &envp[0] = sp + 16 + argc*8 (past argv[0..argc] and its NULL).
            cg.asm.load_imm(SCRATCH, 8);
            cg.asm.mul(2, 0, SCRATCH); // x2 = argc * 8
            cg.asm.add_imm(2, 2, 16); // x2 = argc*8 + 16
            cg.asm.add_imm(SCRATCH, SP, 0); // x8 = sp (ADD #0 reads SP; MOV can't)
            cg.asm.add(2, 2, SCRATCH); // x2 = sp + argc*8 + 16
        }
        cg.asm.bl(main_label); // x0 = Main()
        cg.asm.load_imm(8, 94); // x8 = SYS_exit_group
        cg.asm.svc(); // exit(x0)
    }

    let driver: Vec<&Stmt> = program
        .items
        .iter()
        .filter(|s| !matches!(s.kind, StmtKind::Func(_) | StmtKind::Class(_)))
        .collect();
    cg.emit_function(main_label, &[], &Type::I64, &driver, true, false)?;

    for item in &program.items {
        if let StmtKind::Func(f) = &item.kind {
            if let Some(body) = &f.body {
                let label = cg.funcs[&f.name].label;
                let body_refs: Vec<&Stmt> = body.iter().collect();
                cg.emit_function(label, &f.params, &f.ret, &body_refs, false, f.varargs)?;
            }
        }
    }

    // Freestanding runtime routines used by the program (emitted once, at the end
    // of `__text`, so their forward `bl` references resolve in `finish`).
    if cg.freestanding {
        cg.emit_fs_runtime();
    }

    // Freestanding: there is no symbol table or linker, so the image is the
    // executable. No globals/strings/libc are lowered yet, so any leftover
    // relocation means the program uses a feature not ported to this backend.
    if cg.freestanding {
        let image = cg.asm.finish()?;
        if !image.relocs.is_empty() {
            return Err(CodegenError::new(
                "freestanding aarch64-linux: this program uses a feature (libc call, \
                 global, or formatted output) not yet supported on the freestanding \
                 backend",
                None,
            ));
        }
        return Ok(target.write_executable(&image.text, cg.bss_size));
    }

    // Symbol table: the defined symbols (`_main` + funcs, in __text), then the
    // common globals. The label ids are collected now, but their byte offsets are
    // read from the finished image (post-peephole): `finish` removes dead words and
    // shifts positions, so a pre-`finish` offset would land past the shrunken
    // `__text` and `ld` would warn.
    let mut defined_labels = vec![("_main".to_string(), main_label)];
    for item in &program.items {
        if let StmtKind::Func(f) = &item.kind {
            if f.body.is_some() {
                defined_labels.push((format!("_{}", f.name), cg.funcs[&f.name].label));
            }
        }
    }
    let commons: Vec<(String, u64, u32)> = cg
        .global_order
        .iter()
        .map(|name| {
            let g = &cg.globals[name];
            let size = cg.layouts.size_of(&g.ty).max(1);
            let align_log2 = cg.layouts.align_of(&g.ty).max(1).trailing_zeros();
            (format!("_{name}"), size, align_log2)
        })
        .collect();

    let image = cg.asm.finish()?;
    let defined: Vec<(String, u64)> = defined_labels
        .into_iter()
        .map(|(name, id)| {
            let off = image
                .label_bytes
                .get(id)
                .copied()
                .flatten()
                .ok_or_else(|| CodegenError::new("internal: unplaced function label", None))?;
            Ok((name, off))
        })
        .collect::<Result<_, CodegenError>>()?;
    // Hand the machine code and symbolic relocations to the target's object writer,
    // which lowers the relocations and packages the relocatable object.
    Ok(target.write_object(&image, &defined, &commons, ndefined))
}

/// Compile `program` and produce the executable at `out_path`. For a freestanding
/// target, `compile` already returns the finished executable, so this just writes
/// it and marks it runnable. For a hosted target it writes the relocatable object
/// to a temp file and links it.
fn build(program: &Program, out_path: &Path, target: &dyn ArmTarget) -> Result<(), CodegenError> {
    let obj = compile(program, target)?;
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

// ---- code generation ----

struct FnInfo {
    label: usize,
    params: Vec<Param>,
    ret: Type,
    varargs: bool,
}

/// A local variable's location. Normally the value lives in the frame at
/// `x29 - off`. For an array parameter, which decays to a pointer C-style,
/// `indirect` is set: the slot at `x29 - off` then holds a pointer to the data. A
/// register-promoted local (a non-address-taken scalar; see `plan_registers`)
/// instead lives entirely in a callee-saved register and has no frame slot.
#[derive(Clone)]
struct VarLoc {
    off: u32,
    ty: Type,
    indirect: bool,
    reg: Option<u32>,
}

/// A global variable: a symbol the linker allocates as common storage.
#[derive(Clone)]
struct GlobalInfo {
    sym: u32,
    ty: Type,
}

struct Cg {
    asm: Asm,
    layouts: Layouts,
    funcs: HashMap<String, FnInfo>,
    /// Top-level variables. `order` preserves declaration order for the symtab.
    globals: HashMap<String, GlobalInfo>,
    global_order: Vec<String>,
    scopes: Vec<HashMap<String, VarLoc>>,
    /// Bytes of frame used below x29. `depth` is the current bump pointer, reclaimed
    /// after a call's transient variadic buffer; `max_depth` is the high-water mark
    /// the frame is sized to.
    depth: u32,
    max_depth: u32,
    break_targets: Vec<usize>,
    continue_targets: Vec<usize>,
    labels: HashMap<String, usize>,
    /// Return type of the function currently being emitted (drives F64 returns).
    cur_ret: Type,
    /// Frame offset where the class-return (sret) pointer is saved, if the
    /// current function returns an aggregate by value.
    sret_off: Option<u32>,
    /// Locals promoted to callee-saved registers in the current function:
    /// name -> register (x19..x28). See `plan_registers`.
    promote: HashMap<String, u32>,
    /// Callee-saved registers used by the current function, with where each one's
    /// incoming value is spilled, as `(reg, frame_off)`. Saved in the prologue and
    /// restored in every epilogue.
    cs_saves: Vec<(u32, u32)>,
    /// Whether the program calls `ArgC`/`ArgV`. When set, the `_main` entry
    /// captures the incoming argc/argv (x0/x1) into the hidden globals below.
    uses_args: bool,
    /// Whether the program references `EnvP`. When set, `_main` captures the
    /// environment pointer (the `main` 3rd arg, x2) into `ENVP_GLOBAL`.
    uses_env: bool,
    /// Whether the program calls `MSize` ⇒ `MAlloc` prepends a size header.
    uses_msize: bool,
    /// Whether variadic args go in registers (AAPCS64) vs the stack (Apple).
    variadic_regs: bool,
    /// Whether this is a freestanding target (own `_start`, raw syscalls, no
    /// libc). Selects emitted runtime over libc calls throughout codegen.
    freestanding: bool,
    /// Bump allocator's `(heap_ptr, heap_end)` BSS words, shared by the freestanding
    /// `MAlloc` and `HeapExtend`, allocated once.
    heap_fs_off: Option<(u64, u64)>,
    /// Freestanding BSS layout: each global symbol's byte offset within the BSS
    /// region that follows code+strings, plus the running total size. Globals are
    /// addressed by a self-resolved `ADR` to `text_end + offset`, needing no
    /// relocations. Runtime scratch (allocator state, etc.) is bump-allocated here
    /// too.
    global_bss: HashMap<u32, u64>,
    bss_size: u64,
    /// Freestanding builtin runtime routines (name -> label), emitted once at the
    /// end of `__text` in place of the libc calls the hosted backends make.
    fs_routines: HashMap<&'static str, usize>,
    /// Symbol of the hidden process-global `CTask` region used by the **freestanding**
    /// targets for the implicit `Fs` (single-task; the clone-path per-thread upgrade is
    /// future work). On hosted (Darwin) targets `Fs` is per-thread via pthread TLS, so
    /// this is unused there.
    ctask_sym: Option<u32>,
    /// Whether the program uses `Fs`/exceptions at all (drives the per-thread setup).
    uses_exc: bool,
    /// (Darwin) Symbol of the hidden `pthread_key_t` for the per-thread `CTask`,
    /// created once in `_main`.
    fs_key_sym: Option<u32>,
    /// (Darwin) Frame slot caching this function's `CTask*`, computed once in the
    /// prologue (thread-invariant within a function) so each `Fs` access is a cheap
    /// load instead of a TLS call. `None` if this function never touches `Fs`.
    fs_slot: Option<u32>,
}

/// Hidden globals holding the command line and environment, populated at the entry
/// only when the program uses `ArgC`/`ArgV`/`EnvP`. They are ordinary common symbols.
const ARGC_GLOBAL: &str = "__solomon_holyc_argc";
const ARGV_GLOBAL: &str = "__solomon_holyc_argv";
const ENVP_GLOBAL: &str = "__solomon_holyc_envp";
/// Hidden `CTask` region backing the implicit `Fs` global (exception state).
const CTASK_GLOBAL: &str = "__solomon_holyc_ctask";
/// Hidden `pthread_key_t` for the per-thread `CTask` (Darwin/hosted TLS).
const FSKEY_GLOBAL: &str = "__solomon_holyc_fs_key";
/// `ExcFrame` size on AArch64: prev, saved_sp, saved_fp, landing_pad (4×8=32) plus the
/// callee-saved set saved at `try` and restored by `throw` — x19..x28 (10×8) and
/// d8..d15 (8×8). 32 + 80 + 64 = 176, a 16-byte multiple.
const EXC_FRAME_SIZE: u32 = 176;

impl Cg {
    fn new(layouts: Layouts) -> Self {
        Cg {
            asm: Asm::new(),
            layouts,
            funcs: HashMap::new(),
            globals: HashMap::new(),
            global_order: Vec::new(),
            scopes: Vec::new(),
            depth: 0,
            max_depth: 0,
            break_targets: Vec::new(),
            continue_targets: Vec::new(),
            labels: HashMap::new(),
            cur_ret: Type::I64,
            sret_off: None,
            promote: HashMap::new(),
            cs_saves: Vec::new(),
            uses_args: false,
            uses_env: false,
            uses_msize: false,
            variadic_regs: false,
            freestanding: false,
            heap_fs_off: None,
            global_bss: HashMap::new(),
            bss_size: 0,
            fs_routines: HashMap::new(),
            ctask_sym: None,
            uses_exc: false,
            fs_key_sym: None,
            fs_slot: None,
        }
    }

    /// The label of a freestanding runtime routine, allocated on first use.
    fn fs_routine(&mut self, name: &'static str) -> usize {
        if let Some(&l) = self.fs_routines.get(name) {
            return l;
        }
        let l = self.asm.new_label();
        self.fs_routines.insert(name, l);
        l
    }

    /// Reserve `size` bytes of freestanding BSS at `align`, returning the offset of
    /// its first byte relative to the BSS base (the end of code+strings).
    fn alloc_bss_fs(&mut self, size: u64, align: u64) -> u64 {
        let a = align.max(1);
        let off = self.bss_size.div_ceil(a) * a;
        self.bss_size = off + size.max(1);
        off
    }

    /// Load the address of global `sym` into `dst`. Freestanding: a single
    /// self-resolved `ADR` to its fixed BSS address. Hosted: the relocated
    /// `ADRP`+`ADD` pair the linker fills in.
    fn addr_global(&mut self, dst: u32, sym: u32) {
        if self.freestanding {
            let off = self.global_bss[&sym];
            self.asm.adr_global_fs(dst, off);
        } else {
            self.asm.adrp_global(dst, sym);
            self.asm.add_global(dst, dst, sym);
        }
    }

    /// Load the current task pointer (`CTask *Fs`) into `dst`. Hosted (Darwin): the
    /// per-thread `CTask*` cached in this function's `fs_slot` (a cheap load, no call).
    /// Freestanding: the address of the process-global `CTask` region (single-task).
    fn gen_fs_ptr(&mut self, dst: u32) {
        if self.freestanding {
            // Per-thread: the kernel-managed user thread pointer holds this thread's
            // `CTask` (set at `_start` for the main thread, in the `clone` child for
            // spawned threads).
            self.asm.mrs_tpidr_el0(dst);
        } else {
            let slot = self
                .fs_slot
                .expect("Fs accessed in a function with no cached task slot");
            self.asm.ldur(dst, FP, -(slot as i32));
        }
    }

    /// (Darwin) Compute the current thread's `CTask*` via pthread TLS and cache it in
    /// `slot`. On first access per thread it lazily `malloc`s and zeroes a `CTask` and
    /// `pthread_setspecific`s it, so both the main thread and pthread-spawned threads
    /// get independent exception state with no change to the spawn path.
    fn emit_fs_cache(&mut self, slot: u32) {
        let key = self.fs_key_sym.expect("fs key not registered");
        let size = self.type_size(&Type::Named("CTask".to_string())).max(8);
        let have = self.asm.new_label();
        let done = self.asm.new_label();
        let soff = -(slot as i32);
        // x0 = pthread_getspecific(*key)
        self.addr_global(0, key);
        self.asm.load_mem(0, 0, 8, false);
        self.asm.bl_extern("_pthread_getspecific");
        self.asm.cbnz(0, have);
        // First access on this thread: x0 = malloc(sizeof CTask), zero it, set self.
        self.asm.load_imm(0, size as i64);
        self.asm.bl_extern("_malloc");
        let mut off = 0;
        while off < size {
            self.asm.store_mem_off(31, 0, off, 8); // xzr -> [x0 + off]
            off += 8;
        }
        self.asm.store_mem_off(0, 0, 0, 8); // self-pointer = x0
        self.asm.stur(0, FP, soff); // cache the new task
        // pthread_setspecific(*key, ptr)
        self.addr_global(0, key);
        self.asm.load_mem(0, 0, 8, false); // x0 = key
        self.asm.ldur(1, FP, soff); // x1 = ptr
        self.asm.bl_extern("_pthread_setspecific");
        self.asm.b(done);
        // Existing task on this thread: cache the pointer getspecific returned.
        self.asm.place(have);
        self.asm.stur(0, FP, soff);
        self.asm.place(done);
    }

    /// Byte offset of a `CTask` field (`except_ch`/`catch_except`/`exc_top`).
    fn ctask_off(&self, field: &str) -> u32 {
        self.layouts.offset_of("CTask", field).unwrap_or(0) as u32
    }

    // ---- type helpers ----

    fn type_size(&self, ty: &Type) -> u32 {
        self.layouts.size_of(ty) as u32
    }
    fn type_align(&self, ty: &Type) -> u32 {
        self.layouts.align_of(ty) as u32
    }
    fn expr_ty(&self, e: &Expr) -> Type {
        e.ty().unwrap_or(Type::I64)
    }

    /// Allocate `size` bytes (aligned) below x29; returns the offset to subtract
    /// from x29 for the value's address.
    fn alloc(&mut self, size: u32, align: u32) -> u32 {
        let a = align.max(1);
        self.depth = (self.depth + size).div_ceil(a) * a;
        // `max_depth` is the high-water mark the frame is sized to, so reclaiming
        // `depth` (a call's transient variadic buffer) never shrinks the frame.
        self.max_depth = self.max_depth.max(self.depth);
        self.depth
    }

    fn declare(&mut self, name: &str, off: u32, ty: Type) {
        self.declare_loc(name, off, ty, false);
    }
    fn declare_loc(&mut self, name: &str, off: u32, ty: Type, indirect: bool) {
        // A register-promoted local has no slot (`plan_registers` only promotes
        // direct, non-indirect scalars).
        let reg = if indirect {
            None
        } else {
            self.promote.get(name).copied()
        };
        self.scopes.last_mut().unwrap().insert(
            name.to_string(),
            VarLoc {
                off,
                ty,
                indirect,
                reg,
            },
        );
    }
    fn lookup(&self, name: &str) -> Option<VarLoc> {
        self.scopes.iter().rev().find_map(|s| s.get(name).cloned())
    }

    /// The declared type of a variable (local shadows global).
    fn var_type(&self, name: &str) -> Option<Type> {
        self.lookup(name)
            .map(|v| v.ty)
            .or_else(|| self.globals.get(name).map(|g| g.ty.clone()))
    }

    /// Compute the address of a variable (local or global) into RES.
    fn gen_addr_ident(&mut self, name: &str, pos: Pos) -> Result<(), CodegenError> {
        if let Some(v) = self.lookup(name) {
            self.asm.sub_imm(RES, FP, v.off);
            if v.indirect {
                // The slot holds a pointer to the data (an array parameter).
                self.asm.load_mem(RES, RES, 8, false);
            }
            Ok(())
        } else if let Some(g) = self.globals.get(name) {
            let sym = g.sym;
            self.addr_global(RES, sym);
            Ok(())
        } else {
            Err(CodegenError::at(
                pos,
                format!("undeclared variable `{name}`"),
            ))
        }
    }

    // ---- functions / frame ----

    fn emit_function(
        &mut self,
        entry: usize,
        params: &[Param],
        ret: &Type,
        body: &[&Stmt],
        is_main: bool,
        varargs: bool,
    ) -> Result<(), CodegenError> {
        self.scopes = vec![HashMap::new()];
        self.depth = 0;
        self.max_depth = 0;
        self.break_targets.clear();
        self.continue_targets.clear();
        self.labels.clear();
        self.cur_ret = ret.clone();
        self.sret_off = None;
        self.fs_slot = None;
        self.promote = plan_registers(params, body);
        self.cs_saves.clear();

        for s in body {
            collect_labels(s, self);
        }

        self.asm.place(entry);
        self.asm.stp_pre_fp_lr(); // stp x29,x30,[sp,#-16]!
        self.asm.mov_fp_sp(); // x29 = sp
        let sub_idx = self.asm.emit_sub_sp_placeholder();

        // Spill the incoming value of every callee-saved register we reuse for a
        // promoted local, near x29 (one STUR each), restored in every epilogue. Only
        // distinct registers: with live-range sharing several locals may map to one
        // register, but it is saved and restored once.
        let mut used: Vec<u32> = self.promote.values().copied().collect();
        used.sort_unstable();
        used.dedup();
        for r in used {
            let off = self.alloc(8, 8);
            if is_fp_reg(r) {
                self.asm.fstur(r, FP, -(off as i32));
            } else {
                self.asm.stur(r, FP, -(off as i32));
            }
            self.cs_saves.push((r, off));
        }

        // A by-value aggregate result is written through a caller-supplied
        // pointer in x8 (the indirect result register). Save it before any code
        // can clobber x8 (which doubles as SCRATCH).
        if is_aggregate(ret) {
            let off = self.alloc(8, 8);
            self.asm.sub_imm(T2, FP, off);
            self.asm.store_mem(SCRATCH, T2, 8); // x8 holds the sret pointer
            self.sret_off = Some(off);
        }

        // AAPCS64: integer/pointer params come in x0.., F64 params in v0..,
        // each class numbered independently. A by-value class is passed as a
        // pointer in an integer register; the callee copies it into a local slot.
        let mut igr = 0u32;
        let mut fpr = 0u32;
        for p in params.iter() {
            if matches!(p.ty, Type::Array(..)) {
                // An array parameter decays to a pointer (C-style): the caller
                // passes the array's address in an integer register. Keep the
                // array type for indexing, but mark the slot as indirect.
                if igr > 7 {
                    return Err(CodegenError::at(
                        p.span.pos,
                        "arm64 backend: at most 8 integer parameters",
                    ));
                }
                let off = self.alloc(8, 8);
                self.asm.sub_imm(T2, FP, off);
                self.gen_store(igr, T2, &Type::I64); // store the incoming pointer
                if let Some(name) = &p.name {
                    self.declare_loc(name, off, p.ty.clone(), true);
                }
                igr += 1;
                continue;
            }
            if matches!(p.ty, Type::Named(_)) {
                if igr > 7 {
                    return Err(CodegenError::at(
                        p.span.pos,
                        "arm64 backend: at most 8 integer parameters",
                    ));
                }
                let size = self.type_size(&p.ty);
                let align = self.type_align(&p.ty);
                let off = self.alloc(size.max(1), align);
                self.asm.sub_imm(T2, FP, off);
                self.gen_memcpy(T2, igr, size, SCRATCH); // copy [x_igr] -> slot
                if let Some(name) = &p.name {
                    self.declare(name, off, p.ty.clone());
                }
                igr += 1;
                continue;
            }
            // A promoted parameter goes straight into its callee-saved register
            // with no frame slot: an F64 copied from its v-register, an
            // integer/pointer narrowed from its x-register.
            if let Some(r) = p.name.as_ref().and_then(|n| self.promote.get(n).copied()) {
                if is_f64(&p.ty) {
                    if fpr > 7 {
                        return Err(CodegenError::at(
                            p.span.pos,
                            "arm64 backend: at most 8 floating-point parameters",
                        ));
                    }
                    self.asm.fmov_reg(r, fpr);
                    fpr += 1;
                } else {
                    if igr > 7 {
                        return Err(CodegenError::at(
                            p.span.pos,
                            "arm64 backend: at most 8 integer parameters",
                        ));
                    }
                    self.asm.mov_reg(RES, igr);
                    self.gen_cast(&p.ty); // narrow to the declared width
                    self.asm.mov_reg(r, RES);
                    igr += 1;
                }
                self.declare(p.name.as_ref().unwrap(), 0, p.ty.clone());
                continue;
            }
            let off = self.alloc(8, 8);
            self.asm.sub_imm(T2, FP, off);
            if is_f64(&p.ty) {
                if fpr > 7 {
                    return Err(CodegenError::at(
                        p.span.pos,
                        "arm64 backend: at most 8 floating-point parameters",
                    ));
                }
                self.asm.fmov_to_gpr(RES, fpr);
                self.asm.store_mem(RES, T2, 8);
                fpr += 1;
            } else {
                if igr > 7 {
                    return Err(CodegenError::at(
                        p.span.pos,
                        "arm64 backend: at most 8 integer parameters",
                    ));
                }
                self.gen_store(igr, T2, &p.ty);
                igr += 1;
            }
            if let Some(name) = &p.name {
                self.declare(name, off, p.ty.clone());
            }
        }

        // A variadic function receives two hidden integer params after the named
        // ones (in x{igr}, x{igr+1}): a pointer to the caller's vararg buffer and the
        // count. Spill them to frame slots and expose them as the implicit HolyC
        // varargs locals `I64 *VargV` (the buffer) and `I64 VargC` (the count).
        if varargs {
            if igr + 1 > 7 {
                return Err(CodegenError::at(
                    Pos::new(0, 0),
                    "arm64 backend: too many named params before `...`",
                ));
            }
            let ptr_off = self.alloc(8, 8);
            self.asm.sub_imm(T2, FP, ptr_off);
            self.asm.store_mem(igr, T2, 8); // VargV = va buffer pointer
            let cnt_off = self.alloc(8, 8);
            self.asm.sub_imm(T2, FP, cnt_off);
            self.asm.store_mem(igr + 1, T2, 8); // VargC = count
            self.declare("VargV", ptr_off, Type::Ptr(Box::new(Type::I64)));
            self.declare("VargC", cnt_off, Type::I64);
        }

        // At the entry, AArch64/AAPCS hands `_main` argc in x0 and argv in x1.
        // Stash them in the hidden globals for `ArgC`/`ArgV` (the prologue and the
        // callee-saved spills above don't touch x0/x1).
        if is_main && self.uses_args {
            let csym = self.globals[ARGC_GLOBAL].sym;
            self.addr_global(SCRATCH, csym);
            self.asm.store_mem(0, SCRATCH, 8); // __argc = x0
            let vsym = self.globals[ARGV_GLOBAL].sym;
            self.addr_global(SCRATCH, vsym);
            self.asm.store_mem(1, SCRATCH, 8); // __argv = x1
        }
        // The environment pointer is `main`'s 3rd arg (x2), untouched by the prologue.
        if is_main && self.uses_env {
            let esym = self.globals[ENVP_GLOBAL].sym;
            self.addr_global(SCRATCH, esym);
            self.asm.store_mem(2, SCRATCH, 8); // __envp = x2
        }
        // Per-thread exception state (Darwin/hosted). Done after the argc/env capture
        // (which reads x0/x1/x2) since this clobbers the arg registers. `_main` creates
        // the pthread TLS key once; any function that touches `Fs` then caches this
        // thread's `CTask*` for the rest of its body.
        if !self.freestanding && self.uses_exc {
            if is_main {
                let key = self.fs_key_sym.expect("fs key");
                self.addr_global(0, key); // x0 = &key
                self.asm.load_imm(1, 0); // x1 = NULL destructor
                self.asm.bl_extern("_pthread_key_create");
            }
            if crate::ast::stmts_use_fs_or_exceptions(body) {
                let slot = self.alloc(8, 8);
                self.fs_slot = Some(slot);
                self.emit_fs_cache(slot);
            }
        }

        for &s in body {
            // Top-level declarations are globals (allocated by the linker as
            // common symbols); only their initialisers run here.
            if is_main {
                if let StmtKind::VarDecl { decls } = &s.kind {
                    for d in decls {
                        if let Some(init) = &d.init {
                            self.gen_global_init(d, init)?;
                        }
                    }
                    continue;
                }
            }
            self.gen_stmt(s)?;
        }
        self.asm.load_imm(0, 0);
        self.emit_epilogue();

        let locals = align16(self.max_depth);
        if locals > 4095 {
            return Err(CodegenError::new(
                "arm64 backend: function frame too large (>4 KiB of locals)",
                None,
            ));
        }
        self.asm.patch_sub_sp(sub_idx, locals);
        Ok(())
    }

    fn emit_epilogue(&mut self) {
        // Restore the callee-saved registers we borrowed for promoted locals.
        // (x10 is free here; this never touches x0/d0 holding the return value.)
        for (r, off) in self.cs_saves.clone() {
            if is_fp_reg(r) {
                self.asm.fldur(r, FP, -(off as i32));
            } else {
                self.asm.ldur(r, FP, -(off as i32));
            }
        }
        self.asm.mov_sp_fp(); // sp = x29
        self.asm.ldp_post_fp_lr(); // ldp x29,x30,[sp],#16
        self.asm.ret();
    }

    // ---- statements ----

    fn gen_stmt(&mut self, s: &Stmt) -> Result<(), CodegenError> {
        match &s.kind {
            StmtKind::ShortDecl { .. } => unreachable!("deferred `:=` reached codegen"),
            StmtKind::TypeSwitch { .. } => unreachable!("type switch reached codegen"),
            StmtKind::Empty | StmtKind::Include(_) => {}

            StmtKind::Label(name) => {
                let id = self.labels[name];
                self.asm.place(id);
            }
            StmtKind::Goto(name) => {
                let id = *self.labels.get(name).ok_or_else(|| {
                    CodegenError::at(s.span.pos, format!("unknown label `{name}`"))
                })?;
                self.asm.b(id);
            }

            StmtKind::Expr(e) => self.gen_expr_stmt(e)?,

            StmtKind::Block(stmts) => {
                self.scopes.push(HashMap::new());
                for st in stmts {
                    self.gen_stmt(st)?;
                }
                self.scopes.pop();
            }

            StmtKind::VarDecl { decls } => {
                for d in decls {
                    let size = self.type_size(&d.ty);
                    if is_aggregate(&d.ty) && size == 0 {
                        return Err(CodegenError::at(
                            d.span.pos,
                            "arm64 backend: array size must be a positive constant",
                        ));
                    }
                    // A register-promoted scalar local has no frame slot: evaluate
                    // its initialiser (always a scalar expression), narrow to the
                    // declared width, and move it into the register.
                    if let Some(r) = self.promote.get(&d.name).copied() {
                        self.declare(&d.name, 0, d.ty.clone());
                        if is_f64(&d.ty) {
                            match &d.init {
                                Some(init) => {
                                    self.gen_foperand(init)?; // FRES = value
                                    self.asm.fmov_reg(r, FRES);
                                }
                                None => {
                                    self.asm.load_imm(RES, 0);
                                    self.asm.fmov_from_gpr(r, RES); // 0.0
                                }
                            }
                        } else {
                            match &d.init {
                                Some(init) => {
                                    self.gen_int_expr(init, &d.ty)?;
                                    self.gen_cast(&d.ty);
                                    self.asm.mov_reg(r, RES);
                                }
                                None => self.asm.load_imm(r, 0),
                            }
                        }
                        continue;
                    }
                    let off = self.alloc(size.max(1), self.type_align(&d.ty));
                    self.declare(&d.name, off, d.ty.clone());
                    match &d.init {
                        Some(init) if is_brace_init(init) => {
                            // Brace initialiser (positional or designated): zero
                            // the slot, then store the provided elements/fields
                            // (recursing for nested aggregates).
                            self.gen_zero_slot(off, size);
                            self.gen_init_into(Place::Local(off), &d.ty, 0, init)?;
                        }
                        Some(init) if matches!(d.ty, Type::Named(_)) => {
                            // Copy-initialise a class from another class value.
                            self.gen_expr(init)?; // RES = source address
                            self.asm.sub_imm(T2, FP, off);
                            self.gen_memcpy(T2, RES, size, SCRATCH);
                        }
                        Some(_) if is_aggregate(&d.ty) => {
                            return Err(CodegenError::at(
                                d.span.pos,
                                "arm64 backend: array initializers are not supported",
                            ));
                        }
                        Some(init) => {
                            if is_f64(&d.ty) {
                                self.gen_foperand(init)?;
                                self.asm.fmov_to_gpr(RES, FRES);
                                self.asm.sub_imm(T2, FP, off);
                                self.asm.store_mem(RES, T2, 8);
                            } else {
                                self.gen_int_expr(init, &d.ty)?;
                                self.asm.sub_imm(T2, FP, off);
                                self.gen_store(RES, T2, &d.ty);
                            }
                        }
                        None if !is_aggregate(&d.ty) => {
                            self.asm.load_imm(RES, 0);
                            self.asm.sub_imm(T2, FP, off);
                            self.gen_store(RES, T2, &d.ty);
                        }
                        // An uninitialised aggregate is zero-filled, matching the
                        // interpreter (the conformance oracle). Without this, its
                        // elements/fields would read back as stack garbage.
                        None => self.gen_zero_slot(off, size),
                    }
                }
            }

            StmtKind::If { cond, then, else_ } => {
                crate::backend::gen_if(self, cond, then, else_.as_deref())?
            }

            StmtKind::While { cond, body } => crate::backend::gen_while(self, cond, body)?,

            StmtKind::DoWhile { body, cond } => crate::backend::gen_do_while(self, body, cond)?,

            StmtKind::For {
                init,
                cond,
                step,
                body,
            } => {
                crate::backend::gen_for(self, init.as_deref(), cond.as_ref(), step.as_ref(), body)?
            }

            StmtKind::Switch { cond, body } => self.gen_switch(cond, body, s.span.pos)?,

            StmtKind::Break => {
                let l = *self.break_targets.last().ok_or_else(|| {
                    CodegenError::at(s.span.pos, "`break` outside of a loop/switch")
                })?;
                self.asm.b(l);
            }
            StmtKind::Continue => {
                let l = *self
                    .continue_targets
                    .last()
                    .ok_or_else(|| CodegenError::at(s.span.pos, "`continue` outside of a loop"))?;
                self.asm.b(l);
            }

            StmtKind::Return(val) => {
                match val {
                    Some(e) if is_aggregate(&self.cur_ret) => {
                        // RES = source address of the aggregate. A brace/tuple literal
                        // (`return a, b;`) is built into a temp first; anything else
                        // already denotes an address.
                        let ty = self.cur_ret.clone();
                        if matches!(e.kind, ExprKind::InitList(_)) {
                            let size = self.type_size(&ty).max(1);
                            let align = self.type_align(&ty);
                            let tmp = self.alloc(size, align);
                            self.gen_init_into(Place::Local(tmp), &ty, 0, e)?;
                            self.asm.sub_imm(RES, FP, tmp);
                        } else {
                            self.gen_expr(e)?; // RES = source address
                        }
                        // Copy the aggregate through the saved sret pointer.
                        let off = self.sret_off.expect("aggregate return needs sret slot");
                        self.asm.sub_imm(T2, FP, off);
                        self.asm.load_mem(T2, T2, 8, false); // T2 = sret pointer
                        let n = self.type_size(&ty);
                        self.gen_memcpy(T2, RES, n, SCRATCH);
                    }
                    Some(e) if is_f64(&self.cur_ret) => {
                        self.gen_foperand(e)?; // FRES (converts int -> double if needed)
                        self.asm.fmov_reg(0, FRES); // d0 = result
                    }
                    Some(e) => {
                        // Integer/pointer return; an F64 source converts to the
                        // return type's signedness, then narrows to its width
                        // (C truncates the return value to the return type).
                        let ret = self.cur_ret.clone();
                        self.gen_int_expr(e, &ret)?;
                        self.gen_cast(&ret);
                        self.asm.mov_reg(0, RES);
                    }
                    None => self.asm.load_imm(0, 0),
                }
                self.emit_epilogue();
            }

            StmtKind::Case { .. }
            | StmtKind::Default
            | StmtKind::SwitchStart
            | StmtKind::SwitchEnd => {}

            StmtKind::Try { body, handler } => self.gen_try(body, handler)?,
            StmtKind::Throw(val) => self.gen_throw(val)?,

            StmtKind::Func(_) | StmtKind::Class(_) => {
                return Err(CodegenError::at(
                    s.span.pos,
                    "arm64 backend: nested functions/classes are not supported",
                ));
            }
        }
        Ok(())
    }

    /// Lower a `switch`. The control structure (partition, dispatch, body) comes from
    /// the shared [`crate::backend::gen_switch`] driver. This backend supplies the
    /// leaf emits via [`crate::backend::Emitter`], including the O(1) jump-table fast
    /// path.
    fn gen_switch(&mut self, cond: &Expr, body: &Stmt, pos: Pos) -> Result<(), CodegenError> {
        crate::backend::gen_switch(self, cond, body, pos)
    }

    /// Terminate the process with the exit code already in `x0` (never returns).
    fn emit_exit(&mut self) {
        if self.freestanding {
            self.asm.load_imm(8, 94); // SYS_exit_group
            self.asm.svc();
        } else {
            self.asm.bl_extern("_exit");
        }
    }

    /// Lower `try { body } catch { handler }`. A handler frame (an `ExcFrame`) is built
    /// on the stack and pushed onto the per-task chain (`Fs->exc_top`); the `catch`
    /// block is the longjmp landing pad. On normal completion the frame is popped and
    /// the handler is skipped. See [`Cg::gen_throw`] for the unwinding side. Registers
    /// x11/x12/x13 are caller-saved scratch (x16–x18 are reserved on Darwin).
    fn gen_try(&mut self, body: &[Stmt], handler: &[Stmt]) -> Result<(), CodegenError> {
        let ef = self.alloc(EXC_FRAME_SIZE, 16); // &ExcFrame == FP - ef
        let exctop = self.ctask_off("exc_top");
        let catchex = self.ctask_off("catch_except");
        let catch_l = self.asm.new_label();
        let after_l = self.asm.new_label();

        // x11 = &ExcFrame, x12 = CTask*
        self.asm.sub_imm(11, FP, ef);
        self.gen_fs_ptr(12);
        // frame.prev = Fs->exc_top
        self.asm.load_mem_off(13, 12, exctop, 8, false);
        self.asm.store_mem_off(13, 11, 0, 8);
        // frame.saved_sp = sp ; frame.saved_fp = x29 ; frame.landing_pad = &catch
        self.asm.add_imm(13, SP, 0);
        self.asm.store_mem_off(13, 11, 8, 8);
        self.asm.store_mem_off(FP, 11, 16, 8);
        self.asm.adr_label(13, catch_l);
        self.asm.store_mem_off(13, 11, 24, 8);
        // Save the full callee-saved set (x19..x28, d8..d15) so `throw` can restore the
        // caller's values that abandoned callees may have clobbered.
        for i in 0..10u32 {
            self.asm.stur(19 + i, 11, (32 + i * 8) as i32);
        }
        for i in 0..8u32 {
            self.asm.fstur(8 + i, 11, (112 + i * 8) as i32);
        }
        // Fs->exc_top = &ExcFrame
        self.asm.store_mem_off(11, 12, exctop, 8);

        // try body
        for s in body {
            self.gen_stmt(s)?;
        }

        // Normal completion: pop the handler (Fs->exc_top = frame.prev), skip the catch.
        // The body may have made calls, so recompute the scratch values.
        self.asm.sub_imm(11, FP, ef);
        self.gen_fs_ptr(12);
        self.asm.load_mem_off(13, 11, 0, 8, false);
        self.asm.store_mem_off(13, 12, exctop, 8);
        self.asm.b(after_l);

        // Landing pad: `throw` jumps here with sp/fp/callee-saved already restored,
        // `exc_top` popped, `except_ch` set, and `catch_except` = 1.
        self.asm.place(catch_l);
        for s in handler {
            self.gen_stmt(s)?;
        }
        // Clear catch_except after the handler, matching the interpreter.
        self.gen_fs_ptr(12);
        self.asm.load_imm(13, 0);
        self.asm.store_mem_off(13, 12, catchex, 8);

        self.asm.place(after_l);
        Ok(())
    }

    /// Lower `throw expr;` (or a bare `throw;`). Sets `Fs->except_ch`, then unwinds to
    /// the nearest handler: restores its saved callee-saved set / sp / fp from the
    /// `ExcFrame`, pops it, sets `catch_except` = 1, and branches to its landing pad.
    /// An empty handler chain means an uncaught exception, which exits the process.
    fn gen_throw(&mut self, val: &Option<Expr>) -> Result<(), CodegenError> {
        let except = self.ctask_off("except_ch");
        let exctop = self.ctask_off("exc_top");
        let catchex = self.ctask_off("catch_except");

        // x13 = the value to raise (computed before any further bookkeeping, since the
        // expression may itself call functions and clobber scratch).
        match val {
            Some(e) => {
                self.gen_int_expr(e, &Type::I64)?; // RES
                self.asm.mov_reg(13, RES);
            }
            None => {
                // Bare `throw;` re-raises the current `Fs->except_ch`.
                self.gen_fs_ptr(12);
                self.asm.load_mem_off(13, 12, except, 8, false);
            }
        }
        // Fs->except_ch = x13
        self.gen_fs_ptr(12);
        self.asm.store_mem_off(13, 12, except, 8);
        // x11 = f = Fs->exc_top; if NULL, the exception is uncaught.
        self.asm.load_mem_off(11, 12, exctop, 8, false);
        let live = self.asm.new_label();
        self.asm.cbnz(11, live);
        self.asm.mov_reg(0, 13); // exit code = thrown value
        self.emit_exit();
        self.asm.place(live);
        // Restore the saved callee-saved set, then sp and fp, from the handler frame.
        for i in 0..10u32 {
            self.asm.ldur(19 + i, 11, (32 + i * 8) as i32);
        }
        for i in 0..8u32 {
            self.asm.fldur(8 + i, 11, (112 + i * 8) as i32);
        }
        self.asm.load_mem_off(13, 11, 8, 8, false); // saved_sp
        self.asm.add_imm(SP, 13, 0);
        self.asm.load_mem_off(FP, 11, 16, 8, false); // saved_fp
        // Fs->exc_top = f.prev (x12 still holds CTask*, untouched by the restores).
        self.asm.load_mem_off(13, 11, 0, 8, false);
        self.asm.store_mem_off(13, 12, exctop, 8);
        // catch_except = 1
        self.asm.load_imm(13, 1);
        self.asm.store_mem_off(13, 12, catchex, 8);
        // Jump to the handler's landing pad.
        self.asm.load_mem_off(13, 11, 24, 8, false);
        self.asm.br(13);
        Ok(())
    }

    /// Try to dispatch a switch through an O(1) jump table instead of a linear
    /// compare-chain. Returns `Ok(true)` when it emitted the table, so the caller
    /// skips the compare-chain; `Ok(false)` to fall back.
    ///
    /// Fires only when every `case` value is a compile-time integer constant and the
    /// covered value span is small and dense enough to be worth a table. The table is
    /// `span` 32-bit offset words (`table[k] = label_k - table`). Dispatch is
    /// `idx = v - min`, then an unsigned bounds check, then
    /// `LDRSW off, [table, idx, lsl #2]; BR (table + off)`. Out-of-range and gap
    /// values go to `gap_target` (the switch's default, epilogue, or exit), and
    /// overlapping ranges resolve to the first covering case. Both match the
    /// compare-chain's semantics.
    fn try_gen_branch_table(
        &mut self,
        stmts: &[Stmt],
        label_at: &HashMap<usize, usize>,
        voff: u32,
        gap_target: usize,
    ) -> Result<bool, CodegenError> {
        let mut cases: Vec<(usize, i64, i64)> = Vec::new();
        for (i, st) in stmts.iter().enumerate() {
            if let StmtKind::Case { lo, hi } = &st.kind {
                let Some(lo_v) = const_eval_i64(lo) else {
                    return Ok(false);
                };
                let hi_v = match hi {
                    Some(h) => match const_eval_i64(h) {
                        Some(v) => v,
                        None => return Ok(false),
                    },
                    None => lo_v,
                };
                if hi_v < lo_v {
                    return Ok(false);
                }
                cases.push((label_at[&i], lo_v, hi_v));
            }
        }
        if cases.len() < 4 {
            return Ok(false);
        }
        let min = cases.iter().map(|c| c.1).min().unwrap();
        let max = cases.iter().map(|c| c.2).max().unwrap();
        let span = (max - min + 1) as usize;
        // Bound the table size, and require a reasonable density vs case count.
        if span > 1024 || span > cases.len().saturating_mul(4).max(8) {
            return Ok(false);
        }

        // Map each value to the first case covering it; gaps fall to gap_target.
        let mut slots = vec![gap_target; span];
        let mut filled = vec![false; span];
        for (label, lo, hi) in &cases {
            for v in *lo..=*hi {
                let k = (v - min) as usize;
                if !filled[k] {
                    filled[k] = true;
                    slots[k] = *label;
                }
            }
        }

        self.load_local(RES, voff, &Type::I64);
        if min != 0 {
            self.asm.load_imm(T2, min);
            self.asm.sub(RES, RES, T2); // RES = v - min
        }
        self.asm.load_imm(T2, (span - 1) as i64);
        self.asm.cmp_reg(RES, T2);
        self.asm.b_cond(COND_HI, gap_target); // unsigned out-of-range -> gap
        let table = self.asm.new_label();
        self.asm.adr_label(T2, table); // T2 = &table
        self.asm.ldrsw_reg(SCRATCH, T2, RES); // SCRATCH = table[idx] (signed)
        self.asm.add(T2, T2, SCRATCH); // T2 = &table + offset = target
        self.asm.br(T2); // unconditional: the table data below never runs as code
        self.asm.place(table);
        for slot in slots {
            self.asm.table_word(table, slot);
        }
        Ok(true)
    }

    /// Emit the initialiser store for a global variable.
    fn gen_global_init(&mut self, d: &Declarator, init: &Expr) -> Result<(), CodegenError> {
        let sym = self.globals[&d.name].sym;
        let ty = d.ty.clone();
        if is_brace_init(init) {
            // A global is common storage the linker zeroes, so only the provided
            // elements/fields need stores.
            self.gen_init_into(Place::Global(sym), &ty, 0, init)?;
            return Ok(());
        }
        if matches!(ty, Type::Named(_)) {
            // Copy-initialise a global class from another class value.
            self.gen_expr(init)?; // RES = source address
            self.addr_global(T2, sym);
            let n = self.type_size(&ty);
            self.gen_memcpy(T2, RES, n, SCRATCH);
            return Ok(());
        }
        if is_aggregate(&ty) {
            return Err(CodegenError::at(
                d.span.pos,
                "arm64 backend: array initializers are not supported",
            ));
        }
        if is_f64(&ty) {
            self.gen_foperand(init)?; // value -> FRES
            self.addr_global(T2, sym);
            self.asm.fmov_to_gpr(RES, FRES);
            self.asm.store_mem(RES, T2, 8);
        } else {
            self.gen_int_expr(init, &ty)?; // value -> RES
            self.addr_global(T2, sym);
            self.gen_store(RES, T2, &ty);
        }
        Ok(())
    }

    /// Load the value at `x29 - off` (of type `ty`) into `dst`.
    fn load_local(&mut self, dst: u32, off: u32, ty: &Type) {
        self.asm.sub_imm(dst, FP, off);
        self.gen_load(dst, dst, ty);
    }

    fn gen_load(&mut self, dst: u32, addr: u32, ty: &Type) {
        self.asm
            .load_mem(dst, addr, self.type_size(ty), is_signed(ty));
    }
    fn gen_store(&mut self, val: u32, addr: u32, ty: &Type) {
        self.asm.store_mem(val, addr, self.type_size(ty));
    }

    /// Copy `n` bytes from `[src]` to `[dst]`, using `data` as a scratch GPR.
    /// `dst`, `src` and `data` must be distinct registers.
    fn gen_memcpy(&mut self, dst: u32, src: u32, n: u32, data: u32) {
        let mut o = 0;
        for size in [8u32, 4, 2, 1] {
            while n - o >= size {
                self.asm.load_mem_off(data, src, o, size, false);
                self.asm.store_mem_off(data, dst, o, size);
                o += size;
            }
        }
    }

    /// Address of byte offset `byte_off` within an aggregate at `place`, into `dst`.
    fn elem_addr(&mut self, dst: u32, place: &Place, byte_off: u32) {
        match place {
            // The slot starts at x29 - off; element `byte_off` in is x29 - (off - byte_off).
            Place::Local(off) => self.asm.sub_imm(dst, FP, off - byte_off),
            Place::Global(sym) => {
                self.addr_global(dst, *sym);
                if byte_off > 0 {
                    self.asm.add_imm(dst, dst, byte_off);
                }
            }
        }
    }

    /// Zero `size` bytes of the local slot at `x29 - off`, so a partial brace
    /// initialiser leaves the unset elements zeroed.
    fn gen_zero_slot(&mut self, off: u32, size: u32) {
        self.asm.sub_imm(T2, FP, off); // T2 = slot base
        self.asm.load_imm(RES, 0);
        let mut o = 0;
        for chunk in [8u32, 4, 2, 1] {
            while size - o >= chunk {
                self.asm.store_mem_off(RES, T2, o, chunk);
                o += chunk;
            }
        }
    }

    /// Emit a brace/designated initialiser into the aggregate at `place`. The
    /// recursion and dispatch come from the shared
    /// [`crate::backend::gen_init_into`] driver; this backend supplies the leaf
    /// stores via [`crate::backend::Emitter`].
    fn gen_init_into(
        &mut self,
        place: Place,
        ty: &Type,
        byte_off: u32,
        init: &Expr,
    ) -> Result<(), CodegenError> {
        crate::backend::gen_init_into(self, place, ty, byte_off, init)
    }

    // ---- expressions: value -> RES ----

    /// Evaluate `e` to an integer in RES for storage into a `target`-typed slot.
    /// Identical to `gen_expr` except when converting an F64 source to an unsigned
    /// integer target: it then uses `fcvtzu` instead of the default `fcvtzs`. The two
    /// differ past `I64::MAX` and for negatives. This matches C and the interpreter's
    /// `cast_value`.
    fn gen_int_expr(&mut self, e: &Expr, target: &Type) -> Result<(), CodegenError> {
        if is_unsigned_int(target) && is_f64(&self.expr_ty(e)) {
            self.gen_fexpr(e)?;
            self.asm.fcvtzu(RES, FRES);
            Ok(())
        } else {
            self.gen_expr(e)
        }
    }

    fn gen_expr(&mut self, e: &Expr) -> Result<(), CodegenError> {
        // F64-typed expressions are evaluated into the FP register file. This
        // function's contract is "integer/pointer result in RES", so an F64 value
        // reaching here is in integer context: assignment to an int slot, an int
        // parameter/return, an int array element, and so on. It must be truncated to
        // an integer (matching C and the interpreter), not stored as its raw bit
        // pattern.
        if is_f64(&self.expr_ty(e)) {
            self.gen_fexpr(e)?;
            self.asm.fcvtzs(RES, FRES);
            return Ok(());
        }
        // Constant-fold a compile-time integer expression to a single `load_imm`.
        // `const_eval_i64` mirrors the runtime arithmetic exactly and only succeeds
        // for pure-integer operand trees, so this can't change behavior.
        if matches!(&e.kind, ExprKind::Binary { .. } | ExprKind::Unary { .. }) {
            if let Some(n) = const_eval_i64(e) {
                self.asm.load_imm(RES, n);
                return Ok(());
            }
        }
        let pos = e.span.pos;
        match &e.kind {
            ExprKind::GenericCall { .. } => unreachable!("generic call reached codegen"),
            ExprKind::Int(v) | ExprKind::Char(v) => self.asm.load_imm(RES, *v),
            ExprKind::Float(_) => self.gen_fexpr(e)?,
            ExprKind::Str(s) => {
                let idx = self.asm.intern_string(s);
                self.asm.adr(RES, idx); // a string literal's value is its address
            }
            ExprKind::Ident(name) => self.gen_ident_value(name, pos)?,

            // `*p` reads through the pointer (its type gives the access width).
            ExprKind::Unary {
                op: UnOp::Deref, ..
            } => self.gen_lvalue_value(e)?,
            ExprKind::Unary { op, expr } => self.gen_unary(*op, expr)?,
            ExprKind::Postfix { op, expr } => {
                self.gen_incdec(expr, false, matches!(op, PostOp::Inc))?
            }
            ExprKind::Binary { op, lhs, rhs } => self.gen_binary(*op, lhs, rhs, pos)?,
            ExprKind::Assign { op, target, value } => self.gen_assign(*op, target, value, pos)?,

            ExprKind::Ternary { cond, then, else_ } => {
                self.gen_cond(cond)?;
                let l_else = self.asm.new_label();
                let l_end = self.asm.new_label();
                self.asm.cbz(RES, l_else);
                self.gen_expr(then)?;
                self.asm.b(l_end);
                self.asm.place(l_else);
                self.gen_expr(else_)?;
                self.asm.place(l_end);
            }

            ExprKind::Call { callee, args } => self.gen_call_expr(callee, args)?,

            ExprKind::Index { .. } | ExprKind::Member { .. } => self.gen_lvalue_value(e)?,

            ExprKind::Cast { ty, expr } => {
                // (F64)-typed casts are handled by gen_fexpr; here the target is
                // integer/pointer. A float source needs a real conversion.
                if is_f64(&self.expr_ty(expr)) {
                    self.gen_fexpr(expr)?;
                    if is_unsigned_int(ty) {
                        self.asm.fcvtzu(RES, FRES);
                    } else {
                        self.asm.fcvtzs(RES, FRES);
                    }
                    self.gen_cast(ty); // narrow to the integer width
                } else {
                    self.gen_expr(expr)?;
                    self.gen_cast(ty);
                }
            }
            ExprKind::Sizeof(arg) => {
                let n = match arg {
                    SizeofArg::Type(t) => self.layouts.size_of(t),
                    SizeofArg::Expr(e) => self.layouts.size_of(&self.expr_ty(e)),
                };
                self.asm.load_imm(RES, n as i64);
            }
            ExprKind::Offset { class, path } => {
                let off = self.layouts.nested_offset_of(class, path).ok_or_else(|| {
                    CodegenError::at(pos, format!("cannot compute offset of `{class}`"))
                })?;
                self.asm.load_imm(RES, off as i64);
            }
            ExprKind::InitList(_) => {
                return Err(CodegenError::at(
                    pos,
                    "arm64 backend: an initializer list is only valid as a variable initializer",
                ));
            }
            ExprKind::DesignatedInit(_) => {
                return Err(CodegenError::at(
                    pos,
                    "arm64 backend: a designated initializer is only valid as a variable initializer",
                ));
            }
            ExprKind::Comma(items) => {
                for it in items {
                    self.gen_expr(it)?;
                }
            }
        }
        Ok(())
    }

    fn gen_ident_value(&mut self, name: &str, pos: Pos) -> Result<(), CodegenError> {
        match name {
            "NULL" | "FALSE" => return Ok(self.asm.load_imm(RES, 0)),
            "TRUE" => return Ok(self.asm.load_imm(RES, 1)),
            _ => {}
        }
        // `Fs` is the implicit `CTask *` task pointer: its value is the address of the
        // task region (no load), unless shadowed by a local.
        if name == "Fs" && self.lookup(name).is_none() && self.ctask_sym.is_some() {
            self.gen_fs_ptr(RES);
            return Ok(());
        }
        // `ArgC` (count), `ArgV` (the `U8 **` base), and `EnvP` are implicit globals
        // captured at the entry, unless a user variable shadows them.
        if self.lookup(name).is_none() && matches!(name, "ArgC" | "ArgV" | "EnvP") {
            let g = match name {
                "ArgC" => ARGC_GLOBAL,
                "ArgV" => ARGV_GLOBAL,
                _ => ENVP_GLOBAL,
            };
            let sym = self.globals[g].sym;
            self.addr_global(T2, sym);
            self.asm.load_mem(RES, T2, 8, false);
            return Ok(());
        }
        if self.lookup(name).is_some() || self.globals.contains_key(name) {
            let ty = self.var_type(name).unwrap();
            if is_aggregate(&ty) {
                // An aggregate "value" is its address: arrays decay, and a class
                // is handled by-reference (callers copy as needed).
                return self.gen_addr_ident(name, pos);
            }
            if let Some(r) = self.lookup(name).and_then(|v| v.reg) {
                self.asm.mov_reg(RES, r); // value lives in a callee-saved register
                return Ok(());
            }
            self.gen_addr_ident(name, pos)?;
            self.gen_load(RES, RES, &ty);
            return Ok(());
        }
        if self.funcs.contains_key(name) {
            return self.gen_call(name, &[], pos);
        }
        Err(CodegenError::at(
            pos,
            format!("arm64 backend: `{name}` is undeclared"),
        ))
    }

    /// Load the value of an lvalue expression (Member / Index / Deref).
    fn gen_lvalue_value(&mut self, e: &Expr) -> Result<(), CodegenError> {
        let ty = self.expr_ty(e);
        if is_aggregate(&ty) {
            // Aggregates are represented by their address (arrays decay; structs
            // are passed/copied by-reference).
            return self.gen_addr(e);
        }
        self.gen_addr(e)?;
        self.gen_load(RES, RES, &ty);
        Ok(())
    }

    /// Compute the address of an lvalue into RES.
    fn gen_addr(&mut self, e: &Expr) -> Result<(), CodegenError> {
        let pos = e.span.pos;
        match &e.kind {
            ExprKind::Ident(name) => self.gen_addr_ident(name, pos)?,
            ExprKind::Unary {
                op: UnOp::Deref,
                expr,
            } => {
                self.gen_expr(expr)?; // pointer value IS the address
            }
            ExprKind::Member { base, field, arrow } => {
                let class = if *arrow {
                    self.gen_expr(base)?; // pointer to the class
                    named_of(&self.expr_ty(base).deref_ptr(), pos)?
                } else if is_place(base) {
                    self.gen_addr(base)?;
                    named_of(&self.expr_ty(base), pos)?
                } else {
                    // The base is an aggregate rvalue (e.g. a class-returning
                    // call); its value IS the address of its result temporary.
                    self.gen_expr(base)?;
                    named_of(&self.expr_ty(base), pos)?
                };
                let off = self.layouts.offset_of(&class, field).ok_or_else(|| {
                    CodegenError::at(pos, format!("no field `{field}` on `{class}`"))
                })?;
                if off != 0 {
                    self.asm.add_imm(RES, RES, off as u32);
                }
            }
            ExprKind::Index { base, index } => {
                // Tuple `t[k]` is positional field access `t._k`.
                if let Some(m) = crate::ast::tuple_index_as_member(e) {
                    return self.gen_addr(&m);
                }
                let bty = self.expr_ty(base);
                let elem = bty
                    .elem()
                    .ok_or_else(|| CodegenError::at(pos, "cannot index a non-array/pointer"))?;
                let stride = self.layouts.stride_of(&elem) as i64;
                if matches!(bty, Type::Array(..)) {
                    self.gen_addr(base)?;
                } else {
                    self.gen_expr(base)?; // pointer value
                }
                self.asm.push(RES);
                self.gen_expr(index)?;
                self.asm.pop(T2); // base address
                self.asm.load_imm(SCRATCH, stride);
                self.asm.madd(RES, RES, SCRATCH, T2); // index*stride + base
            }
            _ => return Err(CodegenError::at(pos, "expression is not an lvalue")),
        }
        Ok(())
    }

    fn gen_unary(&mut self, op: UnOp, inner: &Expr) -> Result<(), CodegenError> {
        match op {
            UnOp::Pos => self.gen_expr(inner)?,
            UnOp::Neg => {
                self.gen_expr(inner)?;
                self.asm.neg(RES, RES);
            }
            UnOp::BitNot => {
                self.gen_expr(inner)?;
                self.asm.mvn(RES, RES);
            }
            UnOp::Not => {
                self.gen_cond(inner)?; // RES nonzero iff inner is truthy
                self.asm.cmp_imm0(RES);
                self.asm.cset(RES, COND_EQ);
            }
            UnOp::AddrOf => {
                // `&Func` is the function's code address (a function pointer).
                if let ExprKind::Ident(name) = &inner.kind {
                    if !self.is_variable(name) {
                        if let Some(info) = self.funcs.get(name) {
                            let label = info.label;
                            self.asm.adr_label(RES, label);
                            return Ok(());
                        }
                    }
                }
                self.gen_addr(inner)?
            }
            UnOp::Deref => unreachable!("Deref handled in gen_expr"),
            UnOp::PreInc => self.gen_incdec(inner, true, true)?,
            UnOp::PreDec => self.gen_incdec(inner, true, false)?,
        }
        Ok(())
    }

    /// `++`/`--`, pre or post. Pointers step by the pointee's size.
    fn gen_incdec(&mut self, target: &Expr, pre: bool, inc: bool) -> Result<(), CodegenError> {
        let tty = self.expr_ty(target);
        let delta = match tty.elem() {
            Some(elem) => self.layouts.stride_of(&elem) as u32,
            None => 1,
        };
        if delta > 4095 {
            return Err(CodegenError::at(
                target.span.pos,
                "arm64 backend: pointee too large for ++/--",
            ));
        }
        // Register-promoted target: step the register in place. The result is the
        // new value (pre) or the preserved old value (post). The register always
        // holds the type's narrowed value, so re-narrow after stepping.
        if let Some(r) = self.target_reg(target) {
            if !pre {
                self.asm.mov_reg(T2, r); // keep the old value for the result
            }
            self.asm.mov_reg(RES, r);
            if inc {
                self.asm.add_imm(RES, RES, delta);
            } else {
                self.asm.sub_imm(RES, RES, delta);
            }
            self.gen_cast(&tty);
            self.asm.mov_reg(r, RES);
            if !pre {
                self.asm.mov_reg(RES, T2); // result = old value
            }
            return Ok(());
        }
        self.gen_addr(target)?; // RES = address (no calls after this point)
        self.gen_load(SCRATCH, RES, &tty); // SCRATCH = old value
        self.asm.mov_reg(T2, SCRATCH);
        if inc {
            self.asm.add_imm(T2, T2, delta);
        } else {
            self.asm.sub_imm(T2, T2, delta);
        }
        self.gen_store(T2, RES, &tty);
        self.asm.mov_reg(RES, if pre { T2 } else { SCRATCH });
        Ok(())
    }

    fn gen_binary(
        &mut self,
        op: BinOp,
        lhs: &Expr,
        rhs: &Expr,
        pos: Pos,
    ) -> Result<(), CodegenError> {
        use BinOp::*;
        match op {
            And => return self.gen_logical(lhs, rhs, false),
            Or => return self.gen_logical(lhs, rhs, true),
            _ => {}
        }

        // Pointer arithmetic (the pointer operand's element gives the scale).
        let lt = self.expr_ty(lhs);
        let rt = self.expr_ty(rhs);

        // Floating-point comparison: operands are F64 but the result is an int.
        // (F64 arithmetic is handled in gen_fexpr, since its result type is F64.)
        if matches!(op, Eq | Ne | Lt | Gt | Le | Ge) && (is_f64(&lt) || is_f64(&rt)) {
            self.gen_foperand(lhs)?;
            self.push_f(FRES);
            self.gen_foperand(rhs)?;
            self.pop_f(FT2); // FT2 = lhs, FRES = rhs
            self.asm.fcmp(FT2, FRES);
            let cond = match op {
                Eq => COND_EQ,
                Ne => COND_NE,
                Lt => COND_LT,
                Gt => COND_GT,
                Le => COND_LE,
                Ge => COND_GE,
                _ => unreachable!(),
            };
            self.asm.cset(RES, cond);
            return Ok(());
        }
        if matches!(op, Add | Sub) {
            if let Some(elem) = lt.elem() {
                let stride = self.layouts.stride_of(&elem) as i64;
                if op == Sub && rt.elem().is_some() {
                    // pointer - pointer = element count
                    self.gen_expr(lhs)?;
                    self.asm.push(RES);
                    self.gen_expr(rhs)?;
                    self.asm.pop(T2);
                    self.asm.sub(RES, T2, RES); // byte difference
                    self.asm.load_imm(SCRATCH, stride);
                    self.asm.sdiv(RES, RES, SCRATCH);
                    return Ok(());
                }
                // pointer +/- integer
                self.gen_expr(lhs)?;
                self.asm.push(RES);
                self.gen_expr(rhs)?;
                self.asm.load_imm(SCRATCH, stride);
                self.asm.mul(RES, RES, SCRATCH); // rhs * stride
                self.asm.pop(T2);
                if op == Add {
                    self.asm.add(RES, T2, RES);
                } else {
                    self.asm.sub(RES, T2, RES);
                }
                return Ok(());
            }
            if op == Add && rt.elem().is_some() {
                // integer + pointer
                let stride = self.layouts.stride_of(&rt.elem().unwrap()) as i64;
                self.gen_expr(rhs)?; // pointer
                self.asm.push(RES);
                self.gen_expr(lhs)?; // integer
                self.asm.load_imm(SCRATCH, stride);
                self.asm.mul(RES, RES, SCRATCH);
                self.asm.pop(T2);
                self.asm.add(RES, T2, RES);
                return Ok(());
            }
        }

        // Immediate-form fast path for `<expr> op <small constant>`.
        if self.try_imm_binop(op, lhs, rhs, &lt, &rt)? {
            return Ok(());
        }

        // Evaluate lhs, then rhs, leaving lhs in T2 and rhs in RES. When rhs is a
        // simple operand (its codegen only touches RES), keep lhs in T2 with a
        // register move instead of spilling it to the machine stack.
        self.gen_expr(lhs)?;
        if self.is_simple_operand(rhs) {
            self.asm.mov_reg(T2, RES);
            self.gen_expr(rhs)?;
        } else {
            self.asm.push(RES);
            self.gen_expr(rhs)?;
            self.asm.pop(T2);
        }

        match op {
            Eq | Ne | Lt | Gt | Le | Ge => {
                self.asm.cmp_reg(T2, RES);
                // Relational compares are unsigned if either operand is unsigned
                // (C's usual arithmetic conversions); Eq/Ne don't care.
                let signed = is_signed(&lt) && is_signed(&rt);
                self.asm.cset(RES, cmp_cond(op, signed));
            }
            // Shift signedness follows the left operand's type (default signed).
            _ => {
                let signed = lhs.ty().as_ref().is_none_or(is_signed);
                self.emit_int_binop(op, RES, T2, RES, signed, pos)?;
            }
        }
        Ok(())
    }

    /// Immediate-form fast paths for `<expr> op <small constant>`, and the
    /// commutative `<const> + <expr>`: emit the operation against an immediate
    /// instead of materializing the constant in a register. Returns whether it
    /// handled the op. Fully-constant expressions already fold in `gen_expr`.
    fn try_imm_binop(
        &mut self,
        op: BinOp,
        lhs: &Expr,
        rhs: &Expr,
        lt: &Type,
        rt: &Type,
    ) -> Result<bool, CodegenError> {
        use BinOp::*;
        // Add is commutative, so the constant may be on either side.
        if op == Add {
            let (var, k) = match (const_eval_i64(lhs), const_eval_i64(rhs)) {
                (_, Some(k)) => (lhs, k),
                (Some(k), None) => (rhs, k),
                _ => return Ok(false),
            };
            let Some((sub, imm)) = add_sub_imm12(k) else {
                return Ok(false);
            };
            self.gen_expr(var)?;
            self.emit_addsub_imm(sub, imm);
            return Ok(true);
        }
        // Multiplication by a constant power of two is a left shift. It is
        // commutative and signedness-independent, since both wrap mod 2^64.
        if op == Mul {
            let (var, k) = match (const_eval_i64(lhs), const_eval_i64(rhs)) {
                (_, Some(k)) => (lhs, k),
                (Some(k), None) => (rhs, k),
                _ => return Ok(false),
            };
            let Some(sh) = log2_pow2(k) else {
                return Ok(false);
            };
            self.gen_expr(var)?;
            self.asm.lsl_imm(RES, RES, sh);
            return Ok(true);
        }
        // Every other op only takes a constant right-hand side.
        let Some(k) = const_eval_i64(rhs) else {
            return Ok(false);
        };
        match op {
            Sub => {
                let Some((sub, imm)) = add_sub_imm12(-k) else {
                    return Ok(false);
                };
                self.gen_expr(lhs)?;
                self.emit_addsub_imm(sub, imm);
                Ok(true)
            }
            // Unsigned divide/modulo by a power of two reduce to a logical shift or a
            // low-bits mask. Signed needs a bias to round toward zero, so it keeps the
            // generic SDIV/MSUB path.
            Div if is_unsigned_int(lt) => match log2_pow2(k) {
                Some(sh) => {
                    self.gen_expr(lhs)?;
                    self.asm.lsr_imm(RES, RES, sh);
                    Ok(true)
                }
                None => Ok(false),
            },
            Mod if is_unsigned_int(lt) => match log2_pow2(k) {
                Some(sh) => {
                    self.gen_expr(lhs)?;
                    self.asm.and_imm_lowbits(RES, RES, sh); // x & (2^sh - 1)
                    Ok(true)
                }
                None => Ok(false),
            },
            Eq | Ne | Lt | Gt | Le | Ge if (0..4096).contains(&k) => {
                self.gen_expr(lhs)?;
                self.asm.cmp_imm(RES, k as u32);
                self.asm
                    .cset(RES, cmp_cond(op, is_signed(lt) && is_signed(rt)));
                Ok(true)
            }
            Shl if (0..64).contains(&k) => {
                self.gen_expr(lhs)?;
                if k > 0 {
                    self.asm.lsl_imm(RES, RES, k as u32);
                }
                Ok(true)
            }
            Shr if (0..64).contains(&k) => {
                self.gen_expr(lhs)?;
                if k > 0 {
                    if lhs.ty().as_ref().is_none_or(is_signed) {
                        self.asm.asr_imm(RES, RES, k as u32);
                    } else {
                        self.asm.lsr_imm(RES, RES, k as u32);
                    }
                }
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    /// Emit `RES += imm` (or `RES -= imm`), skipping a zero adjustment.
    fn emit_addsub_imm(&mut self, sub: bool, imm: u32) {
        if imm == 0 {
            return;
        }
        if sub {
            self.asm.sub_imm(RES, RES, imm);
        } else {
            self.asm.add_imm(RES, RES, imm);
        }
    }

    fn gen_logical(&mut self, lhs: &Expr, rhs: &Expr, is_or: bool) -> Result<(), CodegenError> {
        let l_short = self.asm.new_label();
        let l_end = self.asm.new_label();
        self.gen_cond(lhs)?;
        if is_or {
            self.asm.cbnz(RES, l_short);
        } else {
            self.asm.cbz(RES, l_short);
        }
        self.gen_cond(rhs)?;
        self.asm.cmp_imm0(RES);
        self.asm.cset(RES, COND_NE);
        self.asm.b(l_end);
        self.asm.place(l_short);
        self.asm.load_imm(RES, if is_or { 1 } else { 0 });
        self.asm.place(l_end);
        Ok(())
    }

    fn emit_int_binop(
        &mut self,
        op: BinOp,
        rd: u32,
        rn: u32,
        rm: u32,
        signed: bool,
        pos: Pos,
    ) -> Result<(), CodegenError> {
        use BinOp::*;
        match op {
            Add => self.asm.add(rd, rn, rm),
            Sub => self.asm.sub(rd, rn, rm),
            Mul => self.asm.mul(rd, rn, rm),
            // `/` and `%` follow the left operand's signedness (C semantics).
            Div if signed => self.asm.sdiv(rd, rn, rm),
            Div => self.asm.udiv(rd, rn, rm),
            Mod => {
                if signed {
                    self.asm.sdiv(SCRATCH, rn, rm);
                } else {
                    self.asm.udiv(SCRATCH, rn, rm);
                }
                self.asm.msub(rd, SCRATCH, rm, rn);
            }
            BitAnd => self.asm.and(rd, rn, rm),
            BitOr => self.asm.orr(rd, rn, rm),
            BitXor => self.asm.eor(rd, rn, rm),
            Shl => self.asm.lslv(rd, rn, rm),
            // `>>` is arithmetic for a signed left operand, logical for unsigned
            // (C semantics), matching the interpreter.
            Shr if signed => self.asm.asrv(rd, rn, rm),
            Shr => self.asm.lsrv(rd, rn, rm),
            other => {
                return Err(CodegenError::at(
                    pos,
                    format!("arm64 backend: bad binop {other:?}"),
                ));
            }
        }
        Ok(())
    }

    /// The callee-saved register backing `e`, if `e` is a plain reference to a
    /// register-promoted local (so a store can `mov` into it instead of
    /// computing an address).
    fn target_reg(&self, e: &Expr) -> Option<u32> {
        match &e.kind {
            ExprKind::Ident(name) => self.lookup(name).and_then(|v| v.reg),
            _ => None,
        }
    }

    fn gen_assign(
        &mut self,
        op: AssignOp,
        target: &Expr,
        value: &Expr,
        pos: Pos,
    ) -> Result<(), CodegenError> {
        let tty = self.expr_ty(target);
        if op == AssignOp::Assign && is_aggregate(&tty) {
            // Whole-aggregate copy (e.g. class = class).
            self.gen_addr(target)?;
            self.asm.push(RES);
            self.gen_expr(value)?; // RES = source address
            self.asm.pop(T2); // T2 = destination address
            let n = self.type_size(&tty);
            self.gen_memcpy(T2, RES, n, SCRATCH);
            self.asm.mov_reg(RES, T2); // value of the assignment is the dest addr
            return Ok(());
        }
        // A register-promoted target needs no address: compute into RES (the
        // assignment's value), narrow to the declared width, and copy into it.
        if let Some(r) = self.target_reg(target) {
            if op == AssignOp::Assign {
                self.gen_int_expr(value, &tty)?;
            } else {
                self.gen_expr(value)?; // RES = rhs
                self.asm.mov_reg(T2, RES);
                if let (Some(elem), AssignOp::Add | AssignOp::Sub) = (tty.elem(), op) {
                    let stride = self.layouts.stride_of(&elem) as i64;
                    self.asm.load_imm(SCRATCH, stride);
                    self.asm.mul(T2, T2, SCRATCH); // rhs * stride
                    if op == AssignOp::Add {
                        self.asm.add(RES, r, T2);
                    } else {
                        self.asm.sub(RES, r, T2);
                    }
                } else {
                    self.emit_int_binop(compound_binop(op), RES, r, T2, is_signed(&tty), pos)?;
                }
            }
            self.gen_cast(&tty); // the slot path narrows via the store width
            self.asm.mov_reg(r, RES);
            return Ok(());
        }
        if op == AssignOp::Assign {
            self.gen_addr(target)?;
            self.asm.push(RES);
            self.gen_int_expr(value, &tty)?;
            self.asm.pop(T2);
            self.gen_store(RES, T2, &tty);
            return Ok(());
        }
        // Compound assignment.
        self.gen_addr(target)?;
        self.asm.push(RES); // [addr]
        self.gen_load(RES, RES, &tty);
        self.asm.push(RES); // [addr, current]
        self.gen_expr(value)?; // RES = rhs
        self.asm.pop(T2); // current
        if let (Some(elem), AssignOp::Add | AssignOp::Sub) = (tty.elem(), op) {
            // pointer += / -= integer
            let stride = self.layouts.stride_of(&elem) as i64;
            self.asm.load_imm(SCRATCH, stride);
            self.asm.mul(RES, RES, SCRATCH);
            if op == AssignOp::Add {
                self.asm.add(RES, T2, RES);
            } else {
                self.asm.sub(RES, T2, RES);
            }
        } else {
            self.emit_int_binop(compound_binop(op), RES, T2, RES, is_signed(&tty), pos)?;
        }
        self.asm.pop(T2); // addr
        self.gen_store(RES, T2, &tty);
        Ok(())
    }

    fn gen_cast(&mut self, ty: &Type) {
        match ty {
            Type::Bool => {
                self.asm.cmp_imm0(RES);
                self.asm.cset(RES, COND_NE);
            }
            Type::I8 => self.asm.sbfm(RES, RES, 0, 7),
            Type::U8 => self.asm.ubfm(RES, RES, 0, 7),
            Type::I16 => self.asm.sbfm(RES, RES, 0, 15),
            Type::U16 => self.asm.ubfm(RES, RES, 0, 15),
            Type::I32 => self.asm.sbfm(RES, RES, 0, 31),
            Type::U32 => self.asm.ubfm(RES, RES, 0, 31),
            _ => {} // 8-byte / pointer: value already fits
        }
    }

    // ---- floating point (F64) ----

    /// Push the current F64 result (FRES) onto the machine stack, via a GPR.
    fn push_f(&mut self, d: u32) {
        self.asm.fmov_to_gpr(SCRATCH, d);
        self.asm.push(SCRATCH);
    }
    /// Pop the top of the machine stack into a double register, via a GPR.
    fn pop_f(&mut self, d: u32) {
        self.asm.pop(SCRATCH);
        self.asm.fmov_from_gpr(d, SCRATCH);
    }

    /// Evaluate an F64-typed expression; the result lands in FRES.
    fn gen_fexpr(&mut self, e: &Expr) -> Result<(), CodegenError> {
        let pos = e.span.pos;
        match &e.kind {
            ExprKind::Float(v) => {
                self.asm.load_imm(RES, v.to_bits() as i64);
                self.asm.fmov_from_gpr(FRES, RES);
            }
            // An integer literal appearing in float context (e.g. `F64 x = 5;`).
            ExprKind::Int(v) | ExprKind::Char(v) => {
                self.asm.load_imm(RES, *v);
                self.asm.scvtf(FRES, RES);
            }
            ExprKind::Ident(name) => {
                if let Some(r) = self.lookup(name).and_then(|v| v.reg) {
                    self.asm.fmov_reg(FRES, r); // value lives in a callee-saved d-reg
                } else {
                    self.gen_addr_ident(name, pos)?;
                    self.asm.load_mem(RES, RES, 8, false);
                    self.asm.fmov_from_gpr(FRES, RES);
                }
            }
            ExprKind::Unary {
                op: UnOp::Deref, ..
            }
            | ExprKind::Index { .. }
            | ExprKind::Member { .. } => {
                self.gen_addr(e)?;
                self.asm.load_mem(RES, RES, 8, false);
                self.asm.fmov_from_gpr(FRES, RES);
            }
            ExprKind::Unary {
                op: UnOp::Pos,
                expr,
            } => self.gen_fexpr(expr)?,
            ExprKind::Unary {
                op: UnOp::Neg,
                expr,
            } => {
                self.gen_fexpr(expr)?;
                self.asm.fneg(FRES, FRES);
            }
            ExprKind::Binary { op, lhs, rhs } => {
                use BinOp::*;
                if !matches!(op, Add | Sub | Mul | Div) {
                    return Err(CodegenError::at(
                        pos,
                        format!("arm64 backend: operator {op:?} is not supported on F64"),
                    ));
                }
                // Evaluate lhs, then rhs, leaving lhs in FT2 and rhs in FRES. A
                // simple rhs (a literal or scalar that only touches FRES) lets us
                // keep lhs in FT2 with a register move instead of round-tripping it
                // through a GPR and the machine stack (push_f/pop_f).
                self.gen_foperand(lhs)?;
                if self.is_simple_foperand(rhs) {
                    self.asm.fmov_reg(FT2, FRES);
                    self.gen_foperand(rhs)?;
                } else {
                    self.push_f(FRES);
                    self.gen_foperand(rhs)?;
                    self.pop_f(FT2);
                }
                match op {
                    Add => self.asm.fadd(FRES, FT2, FRES),
                    Sub => self.asm.fsub(FRES, FT2, FRES),
                    Mul => self.asm.fmul(FRES, FT2, FRES),
                    Div => self.asm.fdiv(FRES, FT2, FRES),
                    _ => unreachable!(),
                }
            }
            ExprKind::Assign { op, target, value } => self.gen_fassign(*op, target, value, pos)?,
            ExprKind::Ternary { cond, then, else_ } => {
                self.gen_cond(cond)?;
                let l_else = self.asm.new_label();
                let l_end = self.asm.new_label();
                self.asm.cbz(RES, l_else);
                self.gen_fexpr(then)?;
                self.asm.b(l_end);
                self.asm.place(l_else);
                self.gen_fexpr(else_)?;
                self.asm.place(l_end);
            }
            ExprKind::Cast { expr, .. } => {
                // Target is F64 (gen_fexpr is only entered for F64-typed exprs).
                if is_f64(&self.expr_ty(expr)) {
                    self.gen_fexpr(expr)?;
                } else {
                    self.gen_expr(expr)?; // integer in RES
                    self.asm.scvtf(FRES, RES);
                }
            }
            ExprKind::Call { callee, args } => self.gen_call_expr(callee, args)?,
            ExprKind::Comma(items) => {
                for (i, it) in items.iter().enumerate() {
                    if i + 1 == items.len() {
                        self.gen_fexpr(it)?;
                    } else {
                        self.gen_expr(it)?;
                    }
                }
            }
            _ => {
                return Err(CodegenError::at(
                    pos,
                    "arm64 backend: unsupported floating-point expression",
                ));
            }
        }
        Ok(())
    }

    /// Evaluate `e` as a double in FRES, converting from an integer if needed.
    fn gen_foperand(&mut self, e: &Expr) -> Result<(), CodegenError> {
        if is_f64(&self.expr_ty(e)) {
            self.gen_fexpr(e)
        } else {
            self.gen_expr(e)?; // integer in RES
            self.asm.scvtf(FRES, RES);
            Ok(())
        }
    }

    /// Evaluate `e` for use as a boolean test; RES is nonzero iff `e` is true.
    fn gen_cond(&mut self, e: &Expr) -> Result<(), CodegenError> {
        if is_f64(&self.expr_ty(e)) {
            self.gen_fexpr(e)?;
            self.asm.fcmp_zero(FRES);
            self.asm.cset(RES, COND_NE);
        } else {
            self.gen_expr(e)?;
        }
        Ok(())
    }

    /// Assignment where the target is F64. Result (the stored value) in FRES.
    fn gen_fassign(
        &mut self,
        op: AssignOp,
        target: &Expr,
        value: &Expr,
        pos: Pos,
    ) -> Result<(), CodegenError> {
        use BinOp::*;
        // A register-promoted F64 target (its `reg` is a callee-saved d-register)
        // needs no address: compute the new value in FRES and copy it across.
        if let Some(r) = self.target_reg(target) {
            if op == AssignOp::Assign {
                self.gen_foperand(value)?; // FRES = value
            } else {
                let bop = compound_binop(op);
                if !matches!(bop, Add | Sub | Mul | Div) {
                    return Err(CodegenError::at(
                        pos,
                        format!("arm64 backend: operator {bop:?} is not supported on F64"),
                    ));
                }
                self.gen_foperand(value)?; // FRES = rhs
                match bop {
                    Add => self.asm.fadd(FRES, r, FRES),
                    Sub => self.asm.fsub(FRES, r, FRES),
                    Mul => self.asm.fmul(FRES, r, FRES),
                    Div => self.asm.fdiv(FRES, r, FRES),
                    _ => unreachable!(),
                }
            }
            self.asm.fmov_reg(r, FRES);
            return Ok(());
        }
        if op == AssignOp::Assign {
            self.gen_addr(target)?;
            self.asm.push(RES); // [addr]
            self.gen_foperand(value)?;
            self.asm.pop(T2); // addr
            self.asm.fmov_to_gpr(RES, FRES);
            self.asm.store_mem(RES, T2, 8);
            return Ok(());
        }
        // Compound assignment (`+=`, `-=`, `*=`, `/=`).
        let bop = compound_binop(op);
        if !matches!(bop, Add | Sub | Mul | Div) {
            return Err(CodegenError::at(
                pos,
                format!("arm64 backend: operator {bop:?} is not supported on F64"),
            ));
        }
        self.gen_addr(target)?;
        self.asm.push(RES); // [addr]
        self.asm.load_mem(SCRATCH, RES, 8, false);
        self.asm.push(SCRATCH); // [addr, current bits]
        self.gen_foperand(value)?; // FRES = rhs
        self.asm.pop(SCRATCH);
        self.asm.fmov_from_gpr(FT2, SCRATCH); // FT2 = current
        match bop {
            Add => self.asm.fadd(FRES, FT2, FRES),
            Sub => self.asm.fsub(FRES, FT2, FRES),
            Mul => self.asm.fmul(FRES, FT2, FRES),
            Div => self.asm.fdiv(FRES, FT2, FRES),
            _ => unreachable!(),
        }
        self.asm.pop(T2); // addr
        self.asm.fmov_to_gpr(RES, FRES);
        self.asm.store_mem(RES, T2, 8);
        Ok(())
    }

    // ---- calls & printing ----

    fn gen_call(&mut self, name: &str, args: &[Expr], pos: Pos) -> Result<(), CodegenError> {
        // A name that is not a primitive intrinsic is an ordinary function call, even
        // if it shares a name with a former builtin now living in the stdlib (`Sign`,
        // `StrToUpper`, `StrRev`, `StrLen`, …): skip the bespoke lowering and call the
        // compiled body. A compiled user function also shadows a like-named primitive
        // (a program's own `Join`/`Read`). `funcs` holds only real definitions —
        // bodyless lib prototypes aren't compiled — so a present entry means "call the
        // body." Primitive intrinsics (the printf family, heap, clock, sockets,
        // threads) otherwise fall through to the name-keyed lowering below; an
        // optimization intrinsic like `Sqrt` is intercepted earlier, in
        // `gen_call_expr`.
        if self.funcs.contains_key(name) || !crate::intrinsics::is_primitive(name) {
            return self.emit_user_call(name, args, pos);
        }
        // Clock/time primitives. UnixNS = CLOCK_REALTIME, NanoNS = CLOCK_MONOTONIC.
        if name == "UnixNS" || name == "NanoNS" {
            return self.gen_clock(name == "NanoNS", pos);
        }
        if name == "Sleep" {
            return self.gen_sleep(&args[0], pos);
        }
        // POSIX fd primitives. Freestanding lowers to raw Linux syscalls; hosted
        // Darwin calls libc. `Open` needs per-target massaging (an `openat` AT_FDCWD
        // prepend freestanding, a Linux→macOS flag translation on Darwin). The rest
        // map their args straight to the syscall/libc registers.
        if name == "Exit" {
            self.gen_expr(&args[0])?; // RES = code
            self.asm.mov_reg(0, RES); // x0 = code
            if self.freestanding {
                self.asm.load_imm(8, 94); // x8 = SYS_exit_group
                self.asm.svc();
            } else {
                self.asm.bl_extern("_exit"); // libc exit(code), never returns
            }
            return Ok(());
        }
        if matches!(name, "Getpid" | "Getppid" | "Getuid" | "Getgid") {
            if self.freestanding {
                let nr = match name {
                    "Getpid" => 172,  // SYS_getpid
                    "Getppid" => 173, // SYS_getppid
                    "Getuid" => 174,  // SYS_getuid
                    _ => 176,         // SYS_getgid
                };
                self.asm.load_imm(8, nr);
                self.asm.svc();
                self.asm.mov_reg(RES, 0); // id in x0
            } else {
                let sym = match name {
                    "Getpid" => "_getpid",
                    "Getppid" => "_getppid",
                    "Getuid" => "_getuid",
                    _ => "_getgid",
                };
                self.asm.bl_extern(sym);
                self.asm.mov_reg(RES, 0);
                // `pid_t` is a signed int; `uid_t`/`gid_t` are unsigned. Every real id
                // is a small non-negative value, so a 32-bit extend of either
                // signedness yields the same result.
                let ty = if matches!(name, "Getuid" | "Getgid") {
                    Type::U32
                } else {
                    Type::I32
                };
                self.gen_cast(&ty);
            }
            return Ok(());
        }
        if name == "Open" {
            return self.gen_open(args, pos);
        }
        if matches!(
            name,
            "Socket" | "Connect" | "LSeek" | "Read" | "Write" | "Close" | "StdWrite"
        ) {
            // `StdWrite(fd, buf, n)` is `write(fd, buf, n)` on POSIX (the portable
            // stdout/stderr split only matters on Windows, which arm64 doesn't target).
            return self.gen_socket(name, args, pos);
        }
        if matches!(name, "Remove" | "Rename" | "Mkdir" | "Chdir") {
            return self.gen_fsop(name, args, pos);
        }
        if name == "Getcwd" {
            return self.gen_getcwd(args, pos);
        }
        // POSIX-style threads: `pthread_create`/`pthread_join` on Darwin, raw `clone`
        // freestanding.
        if name == "Thread" {
            return self.gen_thread(args, pos);
        }
        if name == "Join" {
            return self.gen_join(args, pos);
        }
        // Atomics (`atomic.hc`): hardware `ldaxr`/`stlxr` loops, acquire/release.
        if matches!(
            name,
            "AtomicLoad" | "AtomicStore" | "AtomicAdd" | "AtomicSwap" | "AtomicCas"
        ) {
            return self.gen_atomic(name, args, pos);
        }
        if name == "AtomicFence" {
            self.asm.dmb_ish();
            return Ok(());
        }
        if matches!(name, "FutexWait" | "FutexWake") {
            return self.gen_futex(name, args, pos);
        }
        // `HeapExtend`: the in-place bump-grow primitive. Freestanding has a real
        // implementation. The hosted libc heap exposes no in-place API, so it returns
        // NULL; `ReAlloc` then takes the copy path, where libc `free` reclaims.
        if name == "HeapExtend" {
            if self.freestanding {
                return self.gen_builtin_fs(name, args, pos);
            }
            for a in args {
                self.gen_expr(a)?; // evaluate for side effects, discard
            }
            self.asm.load_imm(RES, 0);
            return Ok(());
        }
        // `MSize(ptr)`: the requested size from `ptr`'s header (`*(ptr-16)`), or 0 for
        // NULL. Freestanding uses an emitted routine; hosted reads the header inline.
        // Only reached when the program uses `MSize`, so every block has a header.
        if name == "MSize" {
            if self.freestanding {
                return self.gen_builtin_fs(name, args, pos);
            }
            self.gen_expr(&args[0])?; // RES = ptr
            let l_null = self.asm.new_label();
            let l_done = self.asm.new_label();
            self.asm.cbz(RES, l_null);
            self.asm.sub_imm(T2, RES, 16);
            self.asm.load_mem(RES, T2, 8, false); // RES = *(ptr - 16)
            self.asm.b(l_done);
            self.asm.place(l_null);
            self.asm.load_imm(RES, 0);
            self.asm.place(l_done);
            return Ok(());
        }
        // Hosted `MAlloc`/`Free` with the size header (only when the program uses
        // `MSize`): wrap libc `malloc`/`free` so a block carries its requested size in
        // an 8-byte header before the data. Freestanding handles this in its bump
        // routine; a header-free hosted program keeps the plain libc calls below.
        if self.uses_msize && !self.freestanding && name == "MAlloc" {
            self.gen_expr(&args[0])?; // RES = n
            self.asm.add_imm(0, RES, 16); // x0 = n + 16
            self.asm.push(RES); // keep n across the call
            self.asm.bl_extern("_malloc"); // x0 = base
            self.asm.pop(T2); // T2 = n
            self.asm.store_mem(T2, 0, 8); // [base] = n (the size header)
            self.asm.add_imm(RES, 0, 16); // RES = base + 16
            return Ok(());
        }
        if self.uses_msize && !self.freestanding && name == "Free" {
            self.gen_expr(&args[0])?; // RES = ptr
            self.asm.sub_imm(0, RES, 16); // x0 = ptr - 16 (the real block)
            self.asm.bl_extern("_free");
            return Ok(());
        }
        // Freestanding: lower the libc-backed heap primitives to emitted AArch64
        // routines (the `mmap` bump allocator) instead of libc calls.
        if self.freestanding && libc_symbol(name).is_some() {
            return self.gen_builtin_fs(name, args, pos);
        }
        // The hosted (Darwin) heap primitives `MAlloc`/`Free` lower to libc
        // `_malloc`/`_free` (an external call); the argument classes come from the
        // inferred call-site types and the return type is the heap signature.
        if let Some(sym) = libc_symbol(name) {
            let params: Vec<Param> = args
                .iter()
                .map(|a| Param {
                    ty: self.expr_ty(a),
                    name: None,
                    default: None,
                    span: Span::dummy(),
                })
                .collect();
            let ret = Type::I64;
            self.emit_call(
                CallTarget::Extern(sym),
                &params,
                args,
                &ret,
                name,
                pos,
                false,
            )?;
            return Ok(());
        }
        self.emit_user_call(name, args, pos)
    }

    /// Lower a BSD socket primitive (`Socket`/`Connect`/`Read`/`Write`/`Close`).
    /// Freestanding emits the raw Linux syscall (`svc`); hosted Darwin calls the
    /// libc wrapper. Arguments go through the normal ABI lowering. macOS
    /// `socket`/`connect`/`close` return a C `int`, so their result is
    /// sign-extended from `w0`; the Linux syscalls and `read`/`write` already
    /// return a full 64-bit value.
    fn gen_socket(&mut self, name: &str, args: &[Expr], pos: Pos) -> Result<(), CodegenError> {
        let params: Vec<Param> = args
            .iter()
            .map(|a| Param {
                ty: self.expr_ty(a),
                name: None,
                default: None,
                span: Span::dummy(),
            })
            .collect();
        let target = if self.freestanding {
            // aarch64 Linux syscall numbers.
            let nr = match name {
                "Socket" => 198,
                "Connect" => 203,
                "LSeek" => 62,
                "Read" => 63,
                "Write" | "StdWrite" => 64,
                "Close" => 57,
                _ => unreachable!(),
            };
            CallTarget::Syscall(nr)
        } else {
            let sym = match name {
                "Socket" => "_socket",
                "Connect" => "_connect",
                "LSeek" => "_lseek",
                "Read" => "_read",
                "Write" | "StdWrite" => "_write",
                "Close" => "_close",
                _ => unreachable!(),
            };
            CallTarget::Extern(sym)
        };
        self.emit_call(target, &params, args, &Type::I64, name, pos, false)?;
        if !self.freestanding && matches!(name, "Socket" | "Connect" | "Close") {
            self.gen_cast(&Type::I32); // sign-extend the libc `int` return
        }
        Ok(())
    }

    /// Lower `Open(path, flags, mode)`. The `fcntl.hc` flags are Linux's; freestanding
    /// uses `openat(AT_FDCWD, …)` (aarch64 has no bare `open`) with them verbatim,
    /// while Darwin calls libc `open` after translating the flag bits that differ
    /// (`O_CREAT`/`O_TRUNC`/`O_APPEND`) to their macOS values and sign-extending the
    /// `int` result. Args are evaluated onto the stack, then popped into place.
    fn gen_open(&mut self, args: &[Expr], _pos: Pos) -> Result<(), CodegenError> {
        self.gen_expr(&args[0])?; // path
        self.asm.push(RES);
        self.gen_expr(&args[1])?; // RES = flags (Linux values)
        if !self.freestanding {
            // macos = (f & 3) | (O_CREAT 0x40→0x200) | (O_TRUNC 0x200→0x400) |
            //         (O_APPEND 0x400→0x8): each `from`-bit moved to its `to`-bit.
            self.asm.and_imm_lowbits(T2, RES, 2); // access mode (low 2 bits)
            for (from, to) in [(6u32, 9u32), (9, 10), (10, 3)] {
                self.asm.lsr_imm(SCRATCH, RES, from);
                self.asm.and_imm_lowbits(SCRATCH, SCRATCH, 1);
                self.asm.lsl_imm(SCRATCH, SCRATCH, to);
                self.asm.orr(T2, T2, SCRATCH);
            }
            self.asm.mov_reg(RES, T2);
        }
        self.asm.push(RES); // flags
        self.gen_expr(&args[2])?; // mode
        self.asm.push(RES);
        if self.freestanding {
            self.asm.pop(3); // x3 = mode
            self.asm.pop(2); // x2 = flags
            self.asm.pop(1); // x1 = path
            self.asm.load_imm(0, -100); // x0 = AT_FDCWD
            self.asm.load_imm(SCRATCH, 56); // x8 = SYS_openat
            self.asm.svc();
            self.asm.mov_reg(RES, 0); // result (fd / -errno) is in x0
        } else {
            // Apple AArch64 ABI: libc `open` is variadic (`int open(path, oflag,
            // ...)`), so the `mode` arg travels on the stack (the named path/flags go
            // in x0/x1); reserve a 16-byte-aligned slot and place it at `[sp]`.
            self.asm.pop(SCRATCH); // mode
            self.asm.pop(1); // x1 = flags (translated)
            self.asm.pop(0); // x0 = path
            self.asm.sub_sp_imm(16);
            self.asm.str_sp(SCRATCH, 0); // [sp] = mode (first stack vararg)
            self.asm.bl_extern("_open");
            self.asm.add_sp_imm(16);
            self.asm.mov_reg(RES, 0); // open's return is in x0
            self.gen_cast(&Type::I32); // sign-extend the libc `int` return
            // libc `open` returns -1 and stashes the reason in `errno`. The
            // freestanding path already yields `-errno`, so convert here too:
            // `if (ret < 0) ret = -*___error();` (`___error()` is errno's address).
            let ok = self.asm.new_label();
            self.asm.cmp_imm(RES, 0);
            self.asm.b_cond(COND_GE, ok);
            self.asm.bl_extern("___error");
            self.asm.ldr_w(0, 0); // w0 = errno
            self.asm.neg(RES, 0); // RES = -errno
            self.asm.place(ok);
        }
        Ok(())
    }

    /// After a libc call whose `int` result is already in `RES`, convert the `-1`
    /// failure to the `-errno` the freestanding syscalls return:
    /// `if (RES < 0) RES = -*___error();` (`___error()` returns errno's address on
    /// Darwin).
    fn darwin_errno_neg(&mut self) {
        let ok = self.asm.new_label();
        self.asm.cmp_imm(RES, 0);
        self.asm.b_cond(COND_GE, ok);
        self.asm.bl_extern("___error");
        self.asm.ldr_w(0, 0); // w0 = errno
        self.asm.neg(RES, 0); // RES = -errno
        self.asm.place(ok);
    }

    /// Lower the filesystem/working-directory ops `Remove`/`Rename`/`Mkdir`/`Chdir`,
    /// which all return 0 or `-errno`. Freestanding uses the aarch64 `*at` syscalls
    /// (there is no bare `unlink`/`rename`/`mkdir`) with an `AT_FDCWD` prepend; `chdir`
    /// is a bare syscall. Darwin calls libc and converts the `-1`/errno failure to
    /// `-errno`. Args are evaluated onto the stack, then popped into the syscall/ABI
    /// registers.
    fn gen_fsop(&mut self, name: &str, args: &[Expr], _pos: Pos) -> Result<(), CodegenError> {
        for a in args {
            self.gen_expr(a)?;
            self.asm.push(RES);
        }
        if self.freestanding {
            match name {
                "Remove" => {
                    self.asm.pop(1); // x1 = path
                    self.asm.load_imm(0, -100); // x0 = AT_FDCWD
                    self.asm.load_imm(2, 0); // x2 = flags
                    self.asm.load_imm(8, 35); // SYS_unlinkat
                }
                "Chdir" => {
                    self.asm.pop(0); // x0 = path
                    self.asm.load_imm(8, 49); // SYS_chdir (bare)
                }
                "Rename" => {
                    self.asm.pop(3); // x3 = newpath
                    self.asm.pop(1); // x1 = oldpath
                    self.asm.load_imm(0, -100); // x0 = AT_FDCWD (old)
                    self.asm.load_imm(2, -100); // x2 = AT_FDCWD (new)
                    self.asm.load_imm(8, 38); // SYS_renameat
                }
                "Mkdir" => {
                    self.asm.pop(2); // x2 = mode
                    self.asm.pop(1); // x1 = path
                    self.asm.load_imm(0, -100); // x0 = AT_FDCWD
                    self.asm.load_imm(8, 34); // SYS_mkdirat
                }
                _ => unreachable!(),
            }
            self.asm.svc();
            self.asm.mov_reg(RES, 0); // result (0 / -errno) in x0
        } else {
            let sym = match name {
                "Remove" => "_unlink",
                "Rename" => "_rename",
                "Mkdir" => "_mkdir",
                "Chdir" => "_chdir",
                _ => unreachable!(),
            };
            match name {
                "Remove" | "Chdir" => {
                    self.asm.pop(0); // x0 = path
                }
                _ => {
                    self.asm.pop(1); // x1 = newpath / mode
                    self.asm.pop(0); // x0 = oldpath / path
                }
            }
            self.asm.bl_extern(sym);
            self.asm.mov_reg(RES, 0);
            self.gen_cast(&Type::I32); // sign-extend the libc `int` return
            self.darwin_errno_neg(); // -1 → -errno, matching the freestanding path
        }
        Ok(())
    }

    /// Lower `Getcwd(buf, size)` to 0 on success or `-errno`. Freestanding `getcwd`
    /// returns the byte length (incl. NUL) on success, so a non-negative result is
    /// normalised to 0. Darwin libc `getcwd` returns `buf` (non-NULL) or NULL, so a
    /// non-NULL result becomes 0 and NULL becomes `-errno`.
    fn gen_getcwd(&mut self, args: &[Expr], _pos: Pos) -> Result<(), CodegenError> {
        self.gen_expr(&args[0])?; // buf
        self.asm.push(RES);
        self.gen_expr(&args[1])?; // size
        self.asm.push(RES);
        self.asm.pop(1); // x1 = size
        self.asm.pop(0); // x0 = buf
        if self.freestanding {
            self.asm.load_imm(8, 17); // SYS_getcwd
            self.asm.svc(); // x0 = len(>0) or -errno
            self.asm.mov_reg(RES, 0);
            // if RES >= 0 (a length) → 0; a negative -errno passes through.
            let neg = self.asm.new_label();
            self.asm.cmp_imm(RES, 0);
            self.asm.b_cond(COND_LT, neg);
            self.asm.load_imm(RES, 0);
            self.asm.place(neg);
        } else {
            self.asm.bl_extern("_getcwd"); // x0 = buf or NULL
            self.asm.mov_reg(RES, 0);
            // non-NULL → 0; NULL → -errno.
            let done = self.asm.new_label();
            let fail = self.asm.new_label();
            self.asm.cmp_imm(RES, 0);
            self.asm.b_cond(COND_EQ, fail);
            self.asm.load_imm(RES, 0);
            self.asm.b(done);
            self.asm.place(fail);
            self.asm.bl_extern("___error");
            self.asm.ldr_w(0, 0); // w0 = errno
            self.asm.neg(RES, 0); // RES = -errno
            self.asm.place(done);
        }
        Ok(())
    }

    /// Lower `Thread(fn, arg)` → `pthread_create(&tid, NULL, fn, arg)`, returning the
    /// `pthread_t` as the handle. The HolyC thread function `I64 Fn(I64)` matches the
    /// `void *(*)(void *)` start-routine ABI exactly (arg in x0, result in x0), so the
    /// function pointer is passed straight through.
    fn gen_thread(&mut self, args: &[Expr], pos: Pos) -> Result<(), CodegenError> {
        if self.freestanding {
            return self.gen_thread_fs(args, pos);
        }
        let tid_off = self.alloc(8, 8);
        self.gen_expr(&args[0])?; // start routine (the function pointer)
        self.asm.push(RES);
        self.gen_expr(&args[1])?; // arg
        self.asm.push(RES);
        self.asm.pop(3); // x3 = arg
        self.asm.pop(2); // x2 = start routine
        self.asm.sub_imm(0, FP, tid_off); // x0 = &tid
        self.asm.load_imm(1, 0); // x1 = NULL attr
        self.asm.bl_extern("_pthread_create");
        self.load_local(RES, tid_off, &Type::I64); // RES = tid (handle)
        let _ = pos;
        Ok(())
    }

    /// Lower `Join(handle)` → `pthread_join(handle, &retval)`, returning the value the
    /// thread function returned (its `void *` result, read back from `retval`).
    fn gen_join(&mut self, args: &[Expr], pos: Pos) -> Result<(), CodegenError> {
        if self.freestanding {
            return self.gen_join_fs(args, pos);
        }
        let rv_off = self.alloc(8, 8);
        self.gen_expr(&args[0])?; // RES = handle (pthread_t)
        self.asm.mov_reg(0, RES); // x0 = thread
        self.asm.sub_imm(1, FP, rv_off); // x1 = &retval
        self.asm.bl_extern("_pthread_join");
        self.load_local(RES, rv_off, &Type::I64); // RES = retval
        let _ = pos;
        Ok(())
    }

    /// Lower an atomic op (`atomic.hc`), width-directed by the pointer's pointee type.
    /// Load/store use `ldar`/`stlr`; add/swap/cas use `ldaxr`/`stlxr` retry loops. All
    /// are sized 1/2/4/8 bytes. Each loaded value is sign/zero-extended to the pointee
    /// width (`gen_cast`) so the result matches a normal load, and so the add is
    /// correct for a signed narrow type. The pointer lives in T2 and the value
    /// operand(s) in SCRATCH (plus x12 for a CAS expected); the store-exclusive status
    /// is in w11, and the result in RES.
    fn gen_atomic(&mut self, name: &str, args: &[Expr], _pos: Pos) -> Result<(), CodegenError> {
        const STATUS: u32 = 11;
        const EXP: u32 = 12;
        let pty = match self.expr_ty(&args[0]) {
            Type::Ptr(inner) | Type::Array(inner, _) => *inner,
            _ => Type::I64,
        };
        let sz = match self.type_size(&pty) {
            1 => 0,
            2 => 1,
            4 => 2,
            _ => 3, // 8-byte / pointer (and any non-scalar fallback)
        };
        match name {
            "AtomicLoad" => {
                self.gen_expr(&args[0])?; // RES = p
                self.asm.mov_reg(T2, RES);
                self.asm.ldar(RES, T2, sz);
                self.gen_cast(&pty); // sign/zero-extend to the pointee width
            }
            "AtomicStore" => {
                self.gen_expr(&args[1])?; // v
                self.asm.push(RES);
                self.gen_expr(&args[0])?; // p
                self.asm.mov_reg(T2, RES);
                self.asm.pop(RES); // RES = v
                self.asm.stlr(RES, T2, sz); // stores the low `width` bytes
            }
            "AtomicAdd" => {
                self.gen_expr(&args[1])?; // delta
                self.asm.push(RES);
                self.gen_expr(&args[0])?; // p
                self.asm.mov_reg(T2, RES);
                self.asm.pop(SCRATCH); // SCRATCH = delta
                let l = self.asm.new_label();
                self.asm.place(l);
                self.asm.ldaxr(RES, T2, sz); // old
                self.gen_cast(&pty); // extend old (correct add for a signed narrow)
                self.asm.add(RES, RES, SCRATCH); // new = old + delta
                self.asm.stlxr(STATUS, RES, T2, sz);
                self.asm.cbnz(STATUS, l);
                self.gen_cast(&pty); // extend the stored-width result for the return
            }
            "AtomicSwap" => {
                self.gen_expr(&args[1])?; // v
                self.asm.push(RES);
                self.gen_expr(&args[0])?; // p
                self.asm.mov_reg(T2, RES);
                self.asm.pop(SCRATCH); // SCRATCH = v
                let l = self.asm.new_label();
                self.asm.place(l);
                self.asm.ldaxr(RES, T2, sz); // old (the result)
                self.asm.stlxr(STATUS, SCRATCH, T2, sz);
                self.asm.cbnz(STATUS, l);
                self.gen_cast(&pty);
            }
            "AtomicCas" => {
                self.gen_expr(&args[2])?; // desired
                self.asm.push(RES);
                self.gen_expr(&args[1])?; // expected
                self.asm.push(RES);
                self.gen_expr(&args[0])?; // p
                self.asm.mov_reg(T2, RES);
                self.asm.pop(EXP); // x12 = expected
                self.asm.pop(SCRATCH); // SCRATCH = desired
                let l = self.asm.new_label();
                let done = self.asm.new_label();
                self.asm.place(l);
                self.asm.ldaxr(RES, T2, sz); // RES = old (the witnessed value)
                self.gen_cast(&pty); // extend so the compare matches a width value
                self.asm.cmp_reg(RES, EXP);
                self.asm.b_cond(COND_NE, done); // mismatch -> return old, no store
                self.asm.stlxr(STATUS, SCRATCH, T2, sz);
                self.asm.cbnz(STATUS, l); // store lost the monitor -> retry
                self.asm.place(done);
            }
            _ => unreachable!(),
        }
        Ok(())
    }

    /// Lower `FutexWait(addr, val)` / `FutexWake(addr, n)` (`atomic.hc`). Freestanding
    /// uses the Linux `futex(2)` syscall (`FUTEX_WAIT`/`FUTEX_WAKE` on the low 32 bits
    /// of `*addr`); Darwin uses libc `__ulock_wait`/`__ulock_wake`
    /// (`UL_COMPARE_AND_WAIT`). A `FutexWait` carries a short timeout, so a missed
    /// wakeup only turns into a periodic re-check rather than a deadlock. This matches
    /// the documented spurious-wakeup contract: callers always re-test the condition in
    /// a loop.
    fn gen_futex(&mut self, name: &str, args: &[Expr], _pos: Pos) -> Result<(), CodegenError> {
        let wake = name == "FutexWake";
        if self.freestanding {
            self.gen_expr(&args[1])?; // val (expected / n)
            self.asm.push(RES);
            self.gen_expr(&args[0])?; // addr
            self.asm.mov_reg(0, RES); // x0 = uaddr
            self.asm.pop(2); // x2 = val
            self.asm.load_imm(1, if wake { 1 } else { 0 }); // x1 = FUTEX_WAKE / FUTEX_WAIT
            if wake {
                self.asm.load_imm(3, 0); // no timeout for wake
            } else {
                // Relative `struct timespec {0, FUTEX_TIMEOUT_NS}` on the stack -> x3.
                self.asm.sub_sp_imm(16);
                self.asm.load_imm(RES, 0);
                self.asm.str_sp(RES, 0); // tv_sec
                self.asm.load_imm(RES, FUTEX_TIMEOUT_NS);
                self.asm.str_sp(RES, 8); // tv_nsec
                self.asm.add_imm(3, SP, 0); // x3 = &timespec
            }
            self.asm.load_imm(4, 0); // uaddr2
            self.asm.load_imm(5, 0); // val3
            self.asm.load_imm(SCRATCH, 98); // SYS_futex
            self.asm.svc();
            if !wake {
                self.asm.add_sp_imm(16);
            }
            self.asm.mov_reg(RES, 0);
        } else {
            // __ulock_wait(op, addr, value, timeout_us) / __ulock_wake(op, addr, wake_value)
            self.gen_expr(&args[1])?; // value (ignored for wake)
            self.asm.push(RES);
            self.gen_expr(&args[0])?; // addr
            self.asm.mov_reg(1, RES); // x1 = addr
            self.asm.pop(2); // x2 = value
            if wake {
                self.asm.load_imm(2, 0); // wake_value 0 = wake one
                self.asm.load_imm(3, 0);
            } else {
                self.asm.load_imm(3, FUTEX_TIMEOUT_NS / 1000); // timeout in microseconds
            }
            self.asm.load_imm(0, 1); // UL_COMPARE_AND_WAIT
            self.asm.bl_extern(if wake {
                "___ulock_wake"
            } else {
                "___ulock_wait"
            });
            self.asm.mov_reg(RES, 0);
            self.gen_cast(&Type::I32); // libc `int` return
        }
        Ok(())
    }

    /// Freestanding `Thread`: spawn a real `CLONE_THREAD` thread via `clone(2)` onto
    /// an `mmap`'d stack, running `fn(arg)`. A 32-byte thread control block at the
    /// stack base, `[retval | ctid futex | fn | arg]`, passes `fn`/`arg` in and carries
    /// the result back; its address is the handle. `CLONE_PARENT_SETTID` makes the
    /// kernel write the new TID into the `ctid` word, and `CLONE_CHILD_CLEARTID` zeroes
    /// it and futex-wakes on exit, which is how `Join` waits. The child inherits the
    /// register file, so `base` rides in via the callee-saved x19.
    fn gen_thread_fs(&mut self, args: &[Expr], _pos: Pos) -> Result<(), CodegenError> {
        const STACK_SIZE: i64 = 0x2_0000; // 128 KiB stack + TCB
        // CLONE_VM|FS|FILES|SIGHAND|THREAD|PARENT_SETTID|CHILD_CLEARTID. PARENT_SETTID
        // writes the new tid into the futex word synchronously, before clone returns,
        // so `Join` can't race a not-yet-set word. CHILD_CLEARTID zeroes it and
        // futex-wakes on exit.
        const FLAGS: i64 = 0x31_0F00;
        let base_slot = self.alloc(8, 8);
        // mmap(0, SIZE, PROT_READ|WRITE, MAP_PRIVATE|ANON, -1, 0) -> x0 = base.
        self.asm.load_imm(0, 0);
        self.asm.load_imm(1, STACK_SIZE);
        self.asm.load_imm(2, 3); // PROT_READ|PROT_WRITE
        self.asm.load_imm(3, 0x22); // MAP_PRIVATE|MAP_ANONYMOUS
        self.asm.load_imm(4, -1);
        self.asm.load_imm(5, 0);
        self.asm.load_imm(SCRATCH, 222); // SYS_mmap
        self.asm.svc();
        self.asm.sub_imm(T2, FP, base_slot);
        self.asm.store_mem(0, T2, 8); // base_slot = base
        // TCB.fn / TCB.arg.
        self.gen_expr(&args[0])?; // RES = fn (function address)
        self.load_local(T2, base_slot, &Type::I64);
        self.asm.add_imm(T2, T2, 16);
        self.asm.store_mem(RES, T2, 8); // [base+16] = fn
        self.gen_expr(&args[1])?; // RES = arg
        self.load_local(T2, base_slot, &Type::I64);
        self.asm.add_imm(T2, T2, 24);
        self.asm.store_mem(RES, T2, 8); // [base+24] = arg
        // clone(FLAGS, child_sp, ptid=&TCB.futex, tls=0, ctid=&TCB.futex). The arm64
        // arg order is (flags, stack, ptid, tls, ctid).
        let l_child = self.asm.new_label();
        let l_done = self.asm.new_label();
        self.asm.push(19);
        self.load_local(19, base_slot, &Type::I64); // x19 = base (rides into child)
        self.asm.load_imm(SCRATCH, STACK_SIZE - 16);
        self.asm.add(1, 19, SCRATCH); // x1 = child stack top
        self.asm.load_imm(0, FLAGS);
        self.asm.add_imm(2, 19, 8); // x2 = ptid = &TCB.futex (set synchronously)
        self.asm.load_imm(3, 0); // tls
        self.asm.add_imm(4, 19, 8); // x4 = ctid = &TCB.futex (cleared on exit)
        self.asm.load_imm(SCRATCH, 220); // SYS_clone
        self.asm.svc();
        self.asm.cbz(0, l_child);
        // parent: x0 = child tid. The futex word tracks liveness, so nothing to store.
        self.asm.mov_reg(RES, 19); // handle = base
        self.asm.pop(19); // restore the caller's x19
        self.asm.b(l_done);
        // child: fn(arg), stash the return, exit (fires CLONE_CHILD_CLEARTID).
        self.asm.place(l_child);
        // Per-thread exception state: point this child's user thread pointer at a
        // fresh `CTask` carved from its (zero-filled) mmap'd region, just past the
        // 32-byte TCB. Gated on use.
        if self.uses_exc {
            self.asm.add_imm(T2, 19, 32); // T2 = &child CTask (zeroed by MAP_ANONYMOUS)
            self.asm.msr_tpidr_el0(T2);
        }
        self.asm.add_imm(T2, 19, 16);
        self.asm.load_mem(RES, T2, 8, false); // RES = fn
        self.asm.add_imm(T2, 19, 24);
        self.asm.load_mem(0, T2, 8, false); // x0 = arg
        self.asm.blr(RES);
        self.asm.store_mem(0, 19, 8); // [base+0] = retval
        self.asm.load_imm(0, 0);
        self.asm.load_imm(SCRATCH, 93); // SYS_exit (this thread)
        self.asm.svc();
        self.asm.place(l_done);
        Ok(())
    }

    /// Freestanding `Join`: futex-wait on the TCB's `ctid` word until the kernel clears
    /// it (thread exit), then return the `retval` the thread left. The handle is the
    /// TCB address.
    fn gen_join_fs(&mut self, args: &[Expr], _pos: Pos) -> Result<(), CodegenError> {
        self.gen_expr(&args[0])?; // RES = handle (TCB base)
        self.asm.push(19);
        self.asm.mov_reg(19, RES); // x19 = base (preserved across syscalls)
        let l_wait = self.asm.new_label();
        let l_done = self.asm.new_label();
        self.asm.place(l_wait);
        self.asm.add_imm(T2, 19, 8);
        self.asm.load_mem(RES, T2, 8, false); // RES = *ctid (0 once the thread exits)
        self.asm.cbz(RES, l_done);
        // futex(&ctid, FUTEX_WAIT=0, val=*ctid, timeout=NULL).
        self.asm.add_imm(0, 19, 8); // uaddr
        self.asm.load_imm(1, 0); // FUTEX_WAIT
        self.asm.mov_reg(2, RES); // val (the tid we observed)
        self.asm.load_imm(3, 0); // timeout = NULL
        self.asm.load_imm(SCRATCH, 98); // SYS_futex
        self.asm.svc();
        self.asm.b(l_wait);
        self.asm.place(l_done);
        self.asm.load_mem(RES, 19, 8, false); // RES = [base+0] = retval
        self.asm.pop(19); // restore the caller's x19
        Ok(())
    }

    /// Emit a direct call to a user-defined function's compiled body.
    fn emit_user_call(&mut self, name: &str, args: &[Expr], pos: Pos) -> Result<(), CodegenError> {
        let (label, params, ret, varargs) = match self.funcs.get(name) {
            Some(info) => (
                info.label,
                info.params.clone(),
                info.ret.clone(),
                info.varargs,
            ),
            None => {
                return Err(CodegenError::at(
                    pos,
                    format!("arm64 backend: cannot call `{name}` (no compiled body)"),
                ));
            }
        };
        self.emit_call(
            CallTarget::Label(label),
            &params,
            args,
            &ret,
            name,
            pos,
            varargs,
        )
    }

    /// Whether `name` resolves to a variable (local or global) rather than a function.
    /// If so, calling it is an indirect (function-pointer) call.
    fn is_variable(&self, name: &str) -> bool {
        self.lookup(name).is_some() || self.globals.contains_key(name)
    }

    /// Whether evaluating `e` provably touches only RES, never the lhs temp T2, so a
    /// binary op can keep its lhs in T2 rather than spilling it to the stack. Literals,
    /// constant-folded subtrees, and scalar variables qualify; anything that recurses
    /// through T2 or the stack (nested binops, calls, indexing) does not. Both
    /// `gen_addr_ident` and `load_imm` work only through RES.
    fn is_simple_operand(&self, e: &Expr) -> bool {
        if const_eval_i64(e).is_some() {
            return true;
        }
        match &e.kind {
            ExprKind::Int(_) | ExprKind::Char(_) => true,
            ExprKind::Ident(name) => {
                matches!(name.as_str(), "NULL" | "TRUE" | "FALSE") || self.is_variable(name)
            }
            _ => false,
        }
    }

    /// The F64 analogue of `is_simple_operand`: whether evaluating `e` as a float
    /// operand touches only FRES, never the lhs temp FT2. A float literal qualifies, as
    /// does any integer/scalar `is_simple_operand` (which converts to a double through
    /// RES/FRES). An F64 binary op can then keep its lhs in FT2 rather than spilling it
    /// through a GPR and the machine stack.
    fn is_simple_foperand(&self, e: &Expr) -> bool {
        matches!(&e.kind, ExprKind::Float(_)) || self.is_simple_operand(e)
    }

    /// Emit a recognized stdlib intrinsic ([`crate::intrinsics`]) inline: a hardware
    /// instruction in place of a call to the function's lib implementation, where this
    /// backend supports it. Returns whether it was handled. An unhandled name, or one
    /// this backend can't inline, falls through to an ordinary call, so the lib HolyC
    /// body is the fallback. The interpreter always runs that body, and an optimization
    /// intrinsic computes the same value (`Sqrt` is correctly rounded either way), so
    /// conformance holds.
    fn try_intrinsic(
        &mut self,
        name: &str,
        args: &[Expr],
        _pos: Pos,
    ) -> Result<bool, CodegenError> {
        if crate::intrinsics::kind(name).is_none() || args.len() != 1 {
            return Ok(false);
        }
        // Only optimize a call that actually resolves to the lib intrinsic: a single
        // F64 arg, F64 result. A user function that shadows the name with a different
        // signature (e.g. `I64 Trunc(F64)`) must be called normally, since the
        // instruction leaves its result in the FP register, not the integer one.
        if !self.is_f64_unary(name) {
            return Ok(false);
        }
        // Each maps to a single AArch64 FP instruction over the value in FRES.
        let emit: fn(&mut Asm, u32, u32) = match name {
            "Sqrt" => Asm::fsqrt,
            "Fabs" => Asm::fabs,
            "Floor" => Asm::frintm,
            "Ceil" => Asm::frintp,
            "Trunc" => Asm::frintz,
            "Round" => Asm::frinta,
            "RoundToEven" => Asm::frintn,
            _ => return Ok(false),
        };
        self.gen_fexpr(&args[0])?; // FRES = value
        emit(&mut self.asm, FRES, FRES);
        Ok(true)
    }

    /// Whether `name` resolves to a one-argument `F64 -> F64` function: the shape of
    /// the algebraic/rounding intrinsics, so it's safe to replace with the FP
    /// instruction. A user override with a different signature returns `false`.
    fn is_f64_unary(&self, name: &str) -> bool {
        self.funcs.get(name).is_some_and(|f| {
            matches!(f.ret, Type::F64) && f.params.len() == 1 && matches!(f.params[0].ty, Type::F64)
        })
    }

    /// Dispatch a call expression. A bare function or builtin name is a direct call;
    /// anything else (a function-pointer variable or computed value) is indirect.
    fn gen_call_expr(&mut self, callee: &Expr, args: &[Expr]) -> Result<(), CodegenError> {
        let pos = callee.span.pos;
        if let ExprKind::Ident(name) = &callee.kind {
            // Primitive intrinsics (the heap, the clock, sockets, …) get bespoke
            // lowering in `gen_call`. The printf family is ordinary HolyC now
            // (`stdio.hc`), so `gen_call` routes those to their compiled bodies via the
            // shadow check (`funcs.contains_key`).
            if crate::intrinsics::is_primitive(name) {
                return self.gen_call(name, args, pos);
            }
            if !self.is_variable(name) {
                // A recognized stdlib intrinsic the backend lowers inline (e.g.
                // `Sqrt` → `fsqrt`), in place of calling its lib implementation.
                if self.try_intrinsic(name, args, pos)? {
                    return Ok(());
                }
                return self.gen_call(name, args, pos);
            }
        }
        self.gen_indirect_call(callee, args, pos)
    }

    /// Emit an indirect call through a function-pointer value. The callee's
    /// `FuncPtr` type (from sema) drives argument register classing and the
    /// return type.
    fn gen_indirect_call(
        &mut self,
        callee: &Expr,
        args: &[Expr],
        pos: Pos,
    ) -> Result<(), CodegenError> {
        let (ret, ptypes) = match self.expr_ty(callee) {
            Type::FuncPtr { ret, params } => (*ret, params),
            _ => {
                return Err(CodegenError::at(
                    pos,
                    "arm64 backend: called value is not a function pointer",
                ));
            }
        };
        let params: Vec<Param> = ptypes
            .into_iter()
            .map(|ty| Param {
                ty,
                name: None,
                default: None,
                span: Span::dummy(),
            })
            .collect();
        self.emit_call(
            CallTarget::Indirect(callee),
            &params,
            args,
            &ret,
            "<fnptr>",
            pos,
            false,
        )
    }

    /// Emit a call to `target`, passing `args` per `params` (the internal ABI:
    /// integer/pointer args in `x0..`, F64 args in `v0..`, class returns via an
    /// sret pointer in `x8`). Shared by user functions and libc builtins.
    fn emit_call(
        &mut self,
        target: CallTarget,
        params: &[Param],
        args: &[Expr],
        ret: &Type,
        name: &str,
        pos: Pos,
        varargs: bool,
    ) -> Result<(), CodegenError> {
        let n = params.len();
        let indirect_callee = if let CallTarget::Indirect(c) = target {
            Some(c)
        } else {
            None
        };
        // Named args = the declared params, filling a missing trailing arg from its
        // default (an error if none); anything past them is variadic `extra`.
        let mut named: Vec<(&Type, &Expr)> = Vec::with_capacity(n);
        for i in 0..n {
            let arg = if i < args.len() {
                &args[i]
            } else {
                params[i].default.as_ref().ok_or_else(|| {
                    CodegenError::at(pos, format!("missing argument for `{name}`"))
                })?
            };
            named.push((&params[i].ty, arg));
        }
        let extra: Vec<&Expr> = args.get(n..).unwrap_or(&[]).iter().collect();
        // x0.. for integers, v0.. for doubles, numbered independently (arm64: 8 each).
        let classes =
            crate::backend::classify_args(named.iter().map(|(t, _)| *t), 8, 8).map_err(|o| {
                CodegenError::at(
                    pos,
                    match o {
                        crate::backend::ArgOverflow::Int => {
                            "arm64 backend: at most 8 integer arguments"
                        }
                        crate::backend::ArgOverflow::Float => {
                            "arm64 backend: at most 8 floating-point arguments"
                        }
                    },
                )
            })?;
        crate::backend::gen_call(
            self,
            indirect_callee,
            &named,
            &extra,
            &classes,
            ret,
            varargs,
            pos,
            |cg| match target {
                CallTarget::Label(label) => cg.asm.bl(label),
                CallTarget::Extern(sym) => cg.asm.bl_extern(sym),
                CallTarget::Syscall(nr) => {
                    cg.asm.load_imm(SCRATCH, nr); // x8 = syscall number
                    cg.asm.svc();
                }
                CallTarget::Indirect(_) => {
                    // The callee was spilled first, so it is on top of the stack now
                    // that the args have been popped into registers.
                    cg.asm.pop(T2);
                    cg.asm.blr(T2);
                }
            },
        )
    }

    fn gen_expr_stmt(&mut self, e: &Expr) -> Result<(), CodegenError> {
        match &e.kind {
            ExprKind::Str(s) => self.gen_print(s, &[], e.span.pos),
            ExprKind::Comma(items) => {
                if let Some(first) = items.first() {
                    if let ExprKind::Str(fmt) = &first.kind {
                        let fmt = fmt.clone();
                        return self.gen_print(&fmt, &items[1..], e.span.pos);
                    }
                }
                self.gen_expr(e)
            }
            _ => self.gen_expr(e),
        }
    }

    /// Lower a print as a call to the pure-HolyC `Print` (auto-included via `<stdio.hc>`
    /// when a program prints): synthesize `Print(fmt, args…)` and emit it as an
    /// ordinary call to the compiled body. This is target-independent — the same path
    /// serves Darwin and the freestanding ELF, since the HolyC `Print` ultimately calls
    /// `StdWrite`.
    fn gen_print(&mut self, fmt: &str, args: &[Expr], pos: Pos) -> Result<(), CodegenError> {
        let fmt_expr = Expr::new(ExprKind::Str(fmt.to_string()), Span::dummy());
        fmt_expr.set_ty(Type::Ptr(Box::new(Type::U8)));
        let mut call_args = Vec::with_capacity(args.len() + 1);
        call_args.push(fmt_expr);
        call_args.extend(args.iter().cloned());
        self.emit_user_call("Print", &call_args, pos)
    }

    /// Freestanding lowering of a libc-backed builtin: inline scalar ops, or a call
    /// to an emitted runtime routine (same ABI as the libc function it replaces).
    fn gen_builtin_fs(&mut self, name: &str, args: &[Expr], pos: Pos) -> Result<(), CodegenError> {
        // `Sqrt`/`Fabs` are pure HolyC in lib/math.hc now, so there is no inline FP op
        // here. Routine-backed builtins use the standard ABI: args in x0.., result in x0.
        let sname: &'static str = match name {
            "MAlloc" => "MAlloc",
            "Free" => "Free",
            "HeapExtend" => "HeapExtend",
            "MSize" => "MSize",
            other => {
                return Err(CodegenError::at(
                    pos,
                    format!("freestanding aarch64-linux: builtin `{other}` is not supported yet"),
                ));
            }
        };
        let params: Vec<Param> = args
            .iter()
            .map(|a| Param {
                ty: self.expr_ty(a),
                name: None,
                default: None,
                span: Span::dummy(),
            })
            .collect();
        let ret = Type::I64;
        let label = self.fs_routine(sname);
        self.emit_call(
            CallTarget::Label(label),
            &params,
            args,
            &ret,
            name,
            pos,
            false,
        )
    }

    /// Emit the freestanding runtime routines the program uses, once, in a fixed
    /// order. Each follows the internal ABI (args `x0..`, result `x0`) and clobbers
    /// only caller-saved registers, so it is a safe `bl` target.
    fn emit_fs_runtime(&mut self) {
        // The printf family is pure HolyC now (`stdio.hc`), so the formatted-output
        // runtime (`OutWrite`/`FmtInt`/`FmtStr`/`StrLen`) is gone. Only the heap
        // primitives remain emitted; everything else is an ordinary compiled function.
        const ORDER: &[&str] = &["MAlloc", "HeapExtend", "MSize", "Free"];
        for &name in ORDER {
            let Some(&l) = self.fs_routines.get(name) else {
                continue;
            };
            self.asm.place(l);
            match name {
                "MAlloc" => self.emit_fs_malloc(),
                "HeapExtend" => self.emit_fs_heapextend(),
                "MSize" => self.emit_fs_msize(),
                "Free" => self.asm.ret(), // a no-op bump allocator never frees
                _ => {}
            }
        }
    }

    /// The bump allocator's `(heap_ptr, heap_end)` BSS words, allocated once and
    /// shared by `MAlloc` and `HeapExtend`.
    fn heap_globals_fs(&mut self) -> (u64, u64) {
        if let Some(g) = self.heap_fs_off {
            return g;
        }
        let g = (self.alloc_bss_fs(8, 8), self.alloc_bss_fs(8, 8));
        self.heap_fs_off = Some(g);
        g
    }

    /// `MAlloc(x0=n) -> x0`: a bump allocator over `mmap`'d chunks (≥1 MiB,
    /// page-aligned) with 16-byte-aligned allocations and state in two BSS words.
    /// `Free` is a no-op, so chunks are never reused.
    fn emit_fs_malloc(&mut self) {
        let (hp, he) = self.heap_globals_fs(); // heap bump pointer, heap end
        let fits = self.asm.new_label();
        let sized = self.asm.new_label();
        if self.uses_msize {
            self.asm.push(0); // save the original n for the size header
        }
        // x9 = (n + 15) & ~15
        self.asm.add_imm(9, 0, 15);
        self.asm.load_imm(10, -16);
        self.asm.and(9, 9, 10);
        if self.uses_msize {
            self.asm.add_imm(9, 9, 16); // reserve a 16-byte size header
        }
        // x11 = *hp, x12 = *he
        self.asm.adr_global_fs(13, hp);
        self.asm.load_mem(11, 13, 8, false);
        self.asm.adr_global_fs(14, he);
        self.asm.load_mem(12, 14, 8, false);
        self.asm.add(15, 11, 9); // hp + n
        self.asm.cmp_reg(15, 12);
        self.asm.b_cond(COND_LS, fits); // fits in the current chunk
        // chunk size x1 = max(n, 1 MiB), rounded up to a page
        self.asm.mov_reg(1, 9);
        self.asm.load_imm(10, 0x10_0000);
        self.asm.cmp_reg(1, 10);
        self.asm.b_cond(COND_HS, sized);
        self.asm.mov_reg(1, 10);
        self.asm.place(sized);
        self.asm.add_imm(1, 1, 4095);
        self.asm.load_imm(10, -4096);
        self.asm.and(1, 1, 10);
        // mmap(0, x1, PROT_READ|WRITE=3, MAP_PRIVATE|ANON=0x22, -1, 0), nr 222.
        self.asm.load_imm(0, 0);
        self.asm.load_imm(2, 3);
        self.asm.load_imm(3, 0x22);
        self.asm.load_imm(4, -1);
        self.asm.load_imm(5, 0);
        self.asm.load_imm(8, 222);
        self.asm.svc();
        self.asm.mov_reg(11, 0); // hp base = mmap base
        self.asm.add(12, 0, 1); // he = base + chunk size
        self.asm.adr_global_fs(14, he);
        self.asm.store_mem(12, 14, 8);
        self.asm.place(fits);
        // result = x11 (base); *hp = base + n
        self.asm.add(15, 11, 9);
        self.asm.adr_global_fs(13, hp);
        self.asm.store_mem(15, 13, 8);
        if self.uses_msize {
            self.asm.pop(10); // x10 = original n
            self.asm.store_mem(10, 11, 8); // [base] = n (the size header)
            self.asm.add_imm(0, 11, 16); // return base + 16 (past the header)
        } else {
            self.asm.mov_reg(0, 11);
        }
        self.asm.ret();
    }

    /// `HeapExtend(x0=ptr, x1=old, x2=new) -> x0`: extend `ptr` in place to `new`
    /// bytes when it is the last bump-allocated block and still fits the chunk, by
    /// advancing `*heap_ptr` and returning `ptr`; otherwise NULL. It does no
    /// copy/alloc — the move path is the HolyC `ReAlloc`.
    fn emit_fs_heapextend(&mut self) {
        let (hp, he) = self.heap_globals_fs();
        let null = self.asm.new_label();
        self.asm.cbz(0, null); // NULL ptr never extends
        // x9 = align16(old), x11 = align16(new)
        self.asm.add_imm(9, 1, 15);
        self.asm.load_imm(10, -16);
        self.asm.and(9, 9, 10);
        self.asm.add_imm(11, 2, 15);
        self.asm.and(11, 11, 10);
        // last block? ptr + align16(old) == *heap_ptr
        self.asm.add(12, 0, 9); // x12 = block end
        self.asm.adr_global_fs(13, hp);
        self.asm.load_mem(14, 13, 8, false); // x14 = *heap_ptr
        self.asm.cmp_reg(12, 14);
        self.asm.b_cond(COND_NE, null);
        // fits? ptr + align16(new) <= *heap_end
        self.asm.add(12, 0, 11); // x12 = ptr + anew
        self.asm.adr_global_fs(14, he);
        self.asm.load_mem(15, 14, 8, false); // x15 = *heap_end
        self.asm.cmp_reg(12, 15);
        self.asm.b_cond(COND_HI, null); // ptr+anew > heap_end ⇒ doesn't fit
        // extend in place: *heap_ptr = ptr + anew; return ptr (x0 unchanged)
        self.asm.store_mem(12, 13, 8); // x13 still = &heap_ptr
        if self.uses_msize {
            self.asm.sub_imm(9, 0, 16); // x9 = ptr - 16 (header)
            self.asm.store_mem(2, 9, 8); // keep MSize current: [ptr-16] = new size
        }
        self.asm.ret();
        self.asm.place(null);
        self.asm.load_imm(0, 0); // NULL
        self.asm.ret();
    }

    /// `MSize(x0=ptr) -> x0`: the requested byte size from `ptr`'s header
    /// (`*(ptr-16)`), or 0 for NULL. Emitted only when the program uses `MSize`, so
    /// every block carries a header.
    fn emit_fs_msize(&mut self) {
        let null = self.asm.new_label();
        self.asm.cbz(0, null);
        self.asm.sub_imm(9, 0, 16);
        self.asm.load_mem(0, 9, 8, false); // x0 = *(ptr - 16)
        self.asm.ret();
        self.asm.place(null);
        self.asm.load_imm(0, 0); // MSize(NULL) == 0
        self.asm.ret();
    }

    /// `UnixNS()`/`NanoNS()`: read the clock into RES as nanoseconds, either since the
    /// epoch (`mono=false`, CLOCK_REALTIME) or from a monotonic origin (`mono=true`).
    /// Freestanding emits a `clock_gettime` syscall over a BSS timespec; the hosted
    /// (Darwin) target calls libc `clock_gettime` over a stack timespec. The two OSes
    /// disagree on the monotonic clock id (Linux 1, macOS 6).
    fn gen_clock(&mut self, mono: bool, _pos: Pos) -> Result<(), CodegenError> {
        if self.freestanding {
            let ts = self.alloc_bss_fs(16, 8);
            self.asm.load_imm(0, i64::from(mono)); // clockid: 0 realtime / 1 monotonic
            self.asm.adr_global_fs(1, ts);
            self.asm.load_imm(8, 113); // SYS_clock_gettime
            self.asm.svc();
            self.asm.adr_global_fs(9, ts);
            self.asm.load_mem(10, 9, 8, false); // tv_sec  @ +0
            self.asm.ldur(11, 9, 8); // tv_nsec @ +8
        } else {
            self.asm.sub_sp_imm(16); // 16-byte stack timespec (keeps 16-alignment)
            self.asm.load_imm(0, if mono { 6 } else { 0 }); // macOS clock ids
            self.asm.add_imm(1, SP, 0); // x1 = &ts = sp
            self.asm.bl_extern("_clock_gettime");
            self.asm.load_mem(10, SP, 8, false); // tv_sec
            self.asm.ldur(11, SP, 8); // tv_nsec
            self.asm.add_sp_imm(16);
        }
        // RES = tv_sec * 1e9 + tv_nsec
        self.asm.load_imm(12, 1_000_000_000);
        self.asm.mul(10, 10, 12);
        self.asm.add(RES, 10, 11);
        Ok(())
    }

    /// `Sleep(ns)`: suspend the thread for `ns` nanoseconds. Builds a timespec
    /// (`ns / 1e9`, `ns % 1e9`) and issues the `nanosleep` syscall (freestanding) or
    /// libc `nanosleep` (hosted).
    fn gen_sleep(&mut self, arg: &Expr, _pos: Pos) -> Result<(), CodegenError> {
        self.gen_expr(arg)?; // RES = ns
        self.asm.load_imm(10, 1_000_000_000);
        self.asm.udiv(11, RES, 10); // tv_sec  = ns / 1e9
        self.asm.msub(12, 11, 10, RES); // tv_nsec = ns - sec*1e9
        if self.freestanding {
            let ts = self.alloc_bss_fs(16, 8);
            self.asm.adr_global_fs(9, ts);
            self.asm.store_mem(11, 9, 8); // tv_sec  @ +0
            self.asm.stur(12, 9, 8); // tv_nsec @ +8
            self.asm.adr_global_fs(0, ts); // &ts
            self.asm.load_imm(1, 0); // rem = NULL
            self.asm.load_imm(8, 101); // SYS_nanosleep
            self.asm.svc();
        } else {
            self.asm.sub_sp_imm(16);
            self.asm.store_mem(11, SP, 8); // tv_sec
            self.asm.stur(12, SP, 8); // tv_nsec
            self.asm.add_imm(0, SP, 0); // x0 = &ts
            self.asm.load_imm(1, 0); // rem = NULL
            self.asm.bl_extern("_nanosleep");
            self.asm.add_sp_imm(16);
        }
        Ok(())
    }
}

/// Where an aggregate being initialised lives: a local frame slot (`x29 - off`)
/// or a global symbol.
#[derive(Clone, Copy)]
enum Place {
    Local(u32),
    Global(u32),
}

/// This backend's implementation of the shared-driver emitter vtable
/// ([`crate::backend::Emitter`]). It supplies the leaf emits for the shared drivers:
/// the initializer lowering ([`gen_init_into`]); the control-flow drivers (`switch`,
/// where the slot is a frame offset `x29 - off` and arm64 adds a jump-table fast
/// path, plus the loops and conditionals); and the call driver
/// ([`crate::backend::gen_call`]).
impl crate::backend::Emitter for Cg {
    type Place = Place;
    type Slot = u32;

    fn backend_label(&self) -> &'static str {
        "arm64 backend"
    }

    fn init_layouts(&self) -> &Layouts {
        &self.layouts
    }

    fn emit_float_init(
        &mut self,
        place: Place,
        byte_off: u32,
        init: &Expr,
    ) -> Result<(), CodegenError> {
        self.gen_foperand(init)?;
        self.elem_addr(T2, &place, byte_off);
        self.asm.fmov_to_gpr(RES, FRES);
        self.asm.store_mem(RES, T2, 8);
        Ok(())
    }

    fn emit_aggregate_init(
        &mut self,
        place: Place,
        byte_off: u32,
        ty: &Type,
        init: &Expr,
    ) -> Result<(), CodegenError> {
        self.gen_expr(init)?; // RES = source address
        self.elem_addr(T2, &place, byte_off);
        self.gen_memcpy(T2, RES, self.type_size(ty), SCRATCH);
        Ok(())
    }

    fn emit_scalar_init(
        &mut self,
        place: Place,
        byte_off: u32,
        ty: &Type,
        init: &Expr,
    ) -> Result<(), CodegenError> {
        self.gen_int_expr(init, ty)?;
        self.elem_addr(T2, &place, byte_off);
        self.gen_store(RES, T2, ty);
        Ok(())
    }

    fn new_label(&mut self) -> usize {
        self.asm.new_label()
    }
    fn place_label(&mut self, l: usize) {
        self.asm.place(l);
    }
    fn branch(&mut self, l: usize) {
        self.asm.b(l);
    }
    fn push_break(&mut self, l: usize) {
        self.break_targets.push(l);
    }
    fn pop_break(&mut self) {
        self.break_targets.pop();
    }
    fn push_continue(&mut self, l: usize) {
        self.continue_targets.push(l);
    }
    fn pop_continue(&mut self) {
        self.continue_targets.pop();
    }
    fn enter_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }
    fn exit_scope(&mut self) {
        self.scopes.pop();
    }
    fn lower_stmt(&mut self, s: &Stmt) -> Result<(), CodegenError> {
        self.gen_stmt(s)
    }
    fn branch_if_false(&mut self, cond: &Expr, label: usize) -> Result<(), CodegenError> {
        self.gen_expr(cond)?;
        self.asm.cbz(RES, label);
        Ok(())
    }
    fn branch_if_true(&mut self, cond: &Expr, label: usize) -> Result<(), CodegenError> {
        self.gen_expr(cond)?;
        self.asm.cbnz(RES, label);
        Ok(())
    }
    fn eval_expr_discard(&mut self, e: &Expr) -> Result<(), CodegenError> {
        self.gen_expr(e)
    }

    fn eval_switch_value(&mut self, cond: &Expr) -> Result<u32, CodegenError> {
        self.gen_expr(cond)?;
        let voff = self.alloc(8, 8);
        self.asm.sub_imm(T2, FP, voff);
        self.gen_store(RES, T2, &Type::I64);
        Ok(voff)
    }

    fn switch_cmp_branch(
        &mut self,
        slot: u32,
        bound: &Expr,
        cc: crate::backend::SwitchCc,
        target: usize,
    ) -> Result<(), CodegenError> {
        self.gen_expr(bound)?;
        self.asm.mov_reg(T2, RES);
        self.load_local(RES, slot, &Type::I64);
        self.asm.cmp_reg(RES, T2);
        let cond = match cc {
            crate::backend::SwitchCc::Eq => COND_EQ,
            crate::backend::SwitchCc::Lt => COND_LT,
            crate::backend::SwitchCc::Gt => COND_GT,
        };
        self.asm.b_cond(cond, target);
        Ok(())
    }

    fn try_switch_table(
        &mut self,
        stmts: &[Stmt],
        label_at: &HashMap<usize, usize>,
        slot: u32,
        gap_target: usize,
    ) -> Result<bool, CodegenError> {
        self.try_gen_branch_table(stmts, label_at, slot, gap_target)
    }

    // Call-driver leaves: the sret slot is a frame offset; variadics are marshalled by
    // computing the buffer address (`x29 - off`) and count directly into the registers
    // after the named args.
    fn spill_callee(&mut self, callee: &Expr) -> Result<(), CodegenError> {
        self.gen_expr(callee)?; // RES = function address
        self.asm.push(RES);
        Ok(())
    }

    fn alloc_sret(&mut self, ret: &Type) -> Option<u32> {
        // A by-value aggregate result returns through a caller-allocated temp whose
        // address is handed to the callee in x8.
        is_aggregate(ret).then(|| self.alloc(self.type_size(ret).max(1), self.type_align(ret)))
    }

    fn eval_arg_spill(&mut self, ty: &Type, arg: &Expr) -> Result<(), CodegenError> {
        if is_f64(ty) {
            self.gen_foperand(arg)?;
            self.asm.fmov_to_gpr(RES, FRES);
        } else {
            self.gen_int_expr(arg, ty)?;
        }
        self.asm.push(RES);
        Ok(())
    }

    fn place_args(
        &mut self,
        classes: &[crate::backend::ArgClass],
        extra: &[&Expr],
        varargs: bool,
        pos: Pos,
    ) -> Result<(), CodegenError> {
        // Variadic call: stage the trailing args into a frame buffer (8 bytes each, an
        // F64 by its bit pattern). Its address and count become two hidden integer args
        // after the named ones. Always staged for a variadic callee, with count 0 for
        // none; otherwise the callee reads uninitialised registers.
        let va = if varargs {
            let k = extra.len() as u32;
            let off = self.alloc(k * 8, 8);
            for (j, arg) in extra.iter().enumerate() {
                if is_f64(&self.expr_ty(arg)) {
                    self.gen_foperand(arg)?;
                    self.asm.fmov_to_gpr(RES, FRES);
                } else {
                    self.gen_expr(arg)?;
                }
                self.asm.sub_imm(T2, FP, off - (j as u32) * 8);
                self.asm.store_mem(RES, T2, 8);
            }
            Some((off, k))
        } else {
            None
        };
        let igr = classes
            .iter()
            .filter(|c| matches!(c, crate::backend::ArgClass::Int(_)))
            .count() as u32;
        for class in classes.iter().rev() {
            match *class {
                crate::backend::ArgClass::Float(r) => {
                    self.asm.pop(RES);
                    self.asm.fmov_from_gpr(r as u32, RES);
                }
                crate::backend::ArgClass::Int(r) => self.asm.pop(r as u32),
            }
        }
        // The hidden variadic args go in the next two integer registers: `x{igr}` =
        // va_ptr, `x{igr+1}` = va_cnt.
        if let Some((off, k)) = va {
            if igr + 1 > 7 {
                return Err(CodegenError::at(
                    pos,
                    "arm64 backend: too many integer args before `...`",
                ));
            }
            self.asm.sub_imm(igr, FP, off); // va_ptr = &buffer
            self.asm.load_imm(igr + 1, k as i64); // va_cnt
        }
        Ok(())
    }

    fn set_sret_reg(&mut self, slot: Option<u32>) {
        if let Some(off) = slot {
            self.asm.sub_imm(SCRATCH, FP, off); // x8 = &result temp
        }
    }

    fn deliver_result(&mut self, ret: &Type, sret: Option<u32>) {
        if let Some(off) = sret {
            self.asm.sub_imm(RES, FP, off); // result value is the temp's address
        } else if is_f64(ret) {
            self.asm.fmov_reg(FRES, 0); // result in d0
        } else {
            self.asm.mov_reg(RES, 0);
        }
    }

    fn frame_mark(&self) -> u32 {
        self.depth
    }

    fn frame_reset(&mut self, mark: u32) {
        self.depth = mark; // reclaim the call's variadic buffer (max_depth is preserved)
    }
}

/// The callee of an `emit_call`: a local function (resolved by label), an
/// undefined external libc symbol (resolved by the linker), or an indirect call
/// through a function-pointer value (the callee expression).
#[derive(Clone, Copy)]
enum CallTarget<'a> {
    Label(usize),
    Extern(&'static str),
    Indirect(&'a Expr),
    /// A raw Linux `svc #0` syscall with this number in `x8` (freestanding only).
    /// Arguments are passed in `x0..` exactly like the AAPCS register sequence, so
    /// the same `emit_call` argument lowering drives it; the result is in `x0`.
    Syscall(i64),
}

fn is_aggregate(ty: &Type) -> bool {
    matches!(ty, Type::Named(_) | Type::Array(..))
}
/// Whether `e` denotes a place (addressable lvalue) rather than a temporary
/// rvalue. A member of a non-place (e.g. `Mk().x`) must read its base's value,
/// not its address.
fn is_place(e: &Expr) -> bool {
    matches!(
        e.kind,
        ExprKind::Ident(_)
            | ExprKind::Member { .. }
            | ExprKind::Index { .. }
            | ExprKind::Unary {
                op: UnOp::Deref,
                ..
            }
    )
}
/// Whether an initialiser is a brace list (positional or designated), which is
/// stored element-by-element rather than copied as a single value.
fn is_brace_init(e: &Expr) -> bool {
    matches!(e.kind, ExprKind::InitList(_) | ExprKind::DesignatedInit(_))
}
fn is_f64(ty: &Type) -> bool {
    matches!(ty, Type::F64)
}
/// A scalar integer/pointer type can be promoted to a register; aggregates,
/// arrays, F64, and `U0` cannot.
fn is_promotable_scalar(ty: &Type) -> bool {
    matches!(
        ty,
        Type::I8
            | Type::U8
            | Type::I16
            | Type::U16
            | Type::I32
            | Type::U32
            | Type::I64
            | Type::U64
            | Type::Bool
            | Type::Ptr(_)
            | Type::FuncPtr { .. }
    )
}

/// Accumulators for the per-function register-promotion analysis. Alongside the
/// eligibility data (address-taken names, reference counts, declaration count/type),
/// the walk assigns each reference a monotonic program point (`pt`) to build a live
/// interval `[first, last]` per name. It also records each structured loop's point
/// range (for liveness extension) and notes whether the function uses `goto`/labels
/// (`unstructured`, which then disables sharing).
#[derive(Default)]
struct RegAnalysis {
    addr_taken: HashSet<String>,
    refs: HashMap<String, u32>,
    decl_count: HashMap<String, u32>,
    decl_ty: HashMap<String, Type>,
    pt: u32,
    first: HashMap<String, u32>,
    last: HashMap<String, u32>,
    loops: Vec<(u32, u32)>,
    unstructured: bool,
    /// Whether the function contains a `try`. A `throw` longjmps in and restores the
    /// callee-saved registers from the handler's saved set, which would revert any
    /// register-promoted local to its try-entry value — diverging from the interpreter.
    /// So promotion is disabled entirely for such functions (locals stay in memory,
    /// which the sp/fp restore preserves).
    has_try: bool,
    /// Loop-nesting depth at the current scan point. A use inside a loop counts for
    /// more, since it runs every iteration. So loop-invariant reads (a loop bound, say)
    /// get promoted even when their static count is just one.
    loop_depth: u32,
}

impl RegAnalysis {
    /// Record a reference (definition or use) to `name` at the current point,
    /// extending its live interval. `is_use` also bumps the reference count,
    /// weighted by loop depth (`8^depth`, saturating) so hot reads win registers.
    fn touch(&mut self, name: &str, is_use: bool) {
        let p = self.pt;
        self.first.entry(name.to_string()).or_insert(p);
        self.last.insert(name.to_string(), p);
        if is_use {
            let weight = 8u32.saturating_pow(self.loop_depth);
            let slot = self.refs.entry(name.to_string()).or_default();
            *slot = slot.saturating_add(weight);
        }
        self.pt += 1;
    }

    fn scan_stmt(&mut self, s: &Stmt) {
        match &s.kind {
            StmtKind::ShortDecl { .. } => unreachable!("deferred `:=` reached register scan"),
            StmtKind::TypeSwitch { .. } => unreachable!("type switch reached register scan"),
            StmtKind::Empty
            | StmtKind::Break
            | StmtKind::Continue
            | StmtKind::Default
            | StmtKind::SwitchStart
            | StmtKind::SwitchEnd
            | StmtKind::Include(_) => {}
            // Arbitrary control flow can carry a value across an edge the
            // structured-loop analysis doesn't see; disable sharing entirely.
            StmtKind::Goto(_) | StmtKind::Label(_) => self.unstructured = true,
            StmtKind::Expr(e) => self.scan_expr(e),
            StmtKind::Block(stmts) => stmts.iter().for_each(|st| self.scan_stmt(st)),
            StmtKind::VarDecl { decls } => {
                for d in decls {
                    *self.decl_count.entry(d.name.clone()).or_default() += 1;
                    self.decl_ty.insert(d.name.clone(), d.ty.clone());
                    if let Some(init) = &d.init {
                        self.scan_expr(init);
                    }
                    // The declaration defines the variable; record it after the
                    // initializer's uses, so a `t = expr` over disjoint values can
                    // still share (the old value is read before the new is written).
                    self.touch(&d.name, false);
                }
            }
            StmtKind::If { cond, then, else_ } => {
                self.scan_expr(cond);
                self.scan_stmt(then);
                if let Some(e) = else_ {
                    self.scan_stmt(e);
                }
            }
            StmtKind::While { cond, body } => {
                let ls = self.pt;
                self.loop_depth += 1; // cond + body run every iteration
                self.scan_expr(cond);
                self.scan_stmt(body);
                self.loop_depth -= 1;
                self.loops.push((ls, self.pt));
            }
            StmtKind::Switch { cond, body } => {
                self.scan_expr(cond);
                self.scan_stmt(body);
            }
            StmtKind::DoWhile { body, cond } => {
                let ls = self.pt;
                self.loop_depth += 1;
                self.scan_stmt(body);
                self.scan_expr(cond);
                self.loop_depth -= 1;
                self.loops.push((ls, self.pt));
            }
            StmtKind::For {
                init,
                cond,
                step,
                body,
            } => {
                // The init runs once before the loop, so it's outside the range and the
                // depth bump; only cond/step/body repeat each iteration.
                if let Some(i) = init {
                    self.scan_stmt(i);
                }
                let ls = self.pt;
                self.loop_depth += 1;
                if let Some(c) = cond {
                    self.scan_expr(c);
                }
                if let Some(st) = step {
                    self.scan_expr(st);
                }
                self.scan_stmt(body);
                self.loop_depth -= 1;
                self.loops.push((ls, self.pt));
            }
            StmtKind::Case { lo, hi } => {
                self.scan_expr(lo);
                if let Some(h) = hi {
                    self.scan_expr(h);
                }
            }
            StmtKind::Return(v) => {
                if let Some(e) = v {
                    self.scan_expr(e);
                }
            }
            // A `try` disables register promotion for the whole function (see
            // `has_try`): a `throw` restores callee-saved registers on the way in, which
            // would revert a promoted local. Keeping locals in memory matches the
            // interpreter, which never reverts them.
            StmtKind::Try { body, handler } => {
                self.has_try = true;
                body.iter().for_each(|st| self.scan_stmt(st));
                handler.iter().for_each(|st| self.scan_stmt(st));
            }
            StmtKind::Throw(v) => {
                if let Some(e) = v {
                    self.scan_expr(e);
                }
            }
            // A nested function/class has its own scope; its body never refers to
            // this function's locals.
            StmtKind::Func(_) | StmtKind::Class(_) => {}
        }
    }

    fn scan_expr(&mut self, e: &Expr) {
        match &e.kind {
            ExprKind::GenericCall { .. } => unreachable!("generic call reached register scan"),
            ExprKind::Ident(name) => self.touch(name, true),
            ExprKind::Unary {
                op: UnOp::AddrOf,
                expr,
            } => {
                if let ExprKind::Ident(name) = &expr.kind {
                    self.addr_taken.insert(name.clone());
                }
                self.scan_expr(expr);
            }
            ExprKind::Unary { expr, .. }
            | ExprKind::Postfix { expr, .. }
            | ExprKind::Cast { expr, .. } => self.scan_expr(expr),
            ExprKind::Binary { lhs, rhs, .. } => {
                self.scan_expr(lhs);
                self.scan_expr(rhs);
            }
            ExprKind::Assign { target, value, .. } => {
                self.scan_expr(target);
                self.scan_expr(value);
            }
            ExprKind::Ternary { cond, then, else_ } => {
                self.scan_expr(cond);
                self.scan_expr(then);
                self.scan_expr(else_);
            }
            ExprKind::Call { callee, args } => {
                self.scan_expr(callee);
                args.iter().for_each(|a| self.scan_expr(a));
            }
            ExprKind::Index { base, index } => {
                self.scan_expr(base);
                self.scan_expr(index);
            }
            ExprKind::Member { base, .. } => self.scan_expr(base),
            ExprKind::Sizeof(SizeofArg::Expr(ex)) => self.scan_expr(ex),
            ExprKind::InitList(items) | ExprKind::Comma(items) => {
                items.iter().for_each(|i| self.scan_expr(i));
            }
            ExprKind::DesignatedInit(items) => {
                items.iter().for_each(|(_, ex)| self.scan_expr(ex));
            }
            ExprKind::Int(_)
            | ExprKind::Float(_)
            | ExprKind::Str(_)
            | ExprKind::Char(_)
            | ExprKind::Sizeof(SizeofArg::Type(_))
            | ExprKind::Offset { .. } => {}
        }
    }
}

/// One promotion candidate: its (loop-extended) live interval, register file, and
/// reference count (used to prioritize when registers run short).
struct Cand {
    name: String,
    fp: bool,
    start: u32,
    end: u32,
    refs: u32,
}

/// Decide which of a function's scalar locals/params live in callee-saved registers,
/// and which register each gets. Eligible candidates (a non-address-taken scalar
/// declared exactly once and referenced ≥2 times) are allocated by linear scan over
/// their live intervals, so two whose ranges don't overlap share a register. Fewer
/// distinct registers means less prologue/epilogue save/restore, and more than a
/// pool's worth of locals can be promoted. Integer/pointer locals draw from x19..x28,
/// F64 locals from d8..d15.
///
/// Soundness: a live interval is `[first reference, last reference]`, where references
/// include the defining declaration. It is conservatively over-approximated: extended
/// to cover any structured loop it touches (so loop-carried values stay live across
/// the back-edge), and widened to the whole function when the body uses `goto`/labels
/// (then nothing shares). Two variables get the same register only when their
/// intervals are strictly disjoint. Deterministic throughout.
fn plan_registers(params: &[Param], body: &[&Stmt]) -> HashMap<String, u32> {
    let mut a = RegAnalysis::default();
    for p in params {
        if let Some(n) = &p.name {
            *a.decl_count.entry(n.clone()).or_default() += 1;
            a.decl_ty.insert(n.clone(), p.ty.clone());
            // Parameters are live from function entry (point 0).
            a.first.insert(n.clone(), 0);
            a.last.insert(n.clone(), 0);
        }
    }
    a.pt = 1;
    for s in body {
        a.scan_stmt(s);
    }
    let end_pt = a.pt;

    // A function containing a `try` gets no promotion: every local stays in memory so
    // a `throw`'s callee-saved restore can't revert it (see `RegAnalysis::has_try`).
    if a.has_try {
        return HashMap::new();
    }

    let mut cands: Vec<Cand> = Vec::new();
    for (name, &count) in &a.decl_count {
        let refs = a.refs.get(name).copied().unwrap_or(0);
        if count != 1 || a.addr_taken.contains(name) || refs < 2 {
            continue;
        }
        let fp = match a.decl_ty.get(name) {
            Some(ty) if is_promotable_scalar(ty) => false,
            Some(ty) if is_f64(ty) => true,
            _ => continue,
        };
        let (start, end) = if a.unstructured {
            (0, end_pt) // unstructured control flow: live the whole function
        } else {
            (
                a.first.get(name).copied().unwrap_or(0),
                a.last.get(name).copied().unwrap_or(0),
            )
        };
        cands.push(Cand {
            name: name.clone(),
            fp,
            start,
            end,
            refs,
        });
    }

    // Extend each interval to cover any loop it intersects, to a fixpoint so nested
    // loops widen outward. Loop-carried values are live across the back-edge, beyond
    // their textual last reference.
    if !a.unstructured {
        loop {
            let mut changed = false;
            for c in &mut cands {
                for &(ls, le) in &a.loops {
                    if c.start <= le && c.end >= ls {
                        if c.start > ls {
                            c.start = ls;
                            changed = true;
                        }
                        if c.end < le {
                            c.end = le;
                            changed = true;
                        }
                    }
                }
            }
            if !changed {
                break;
            }
        }
    }

    // Linear scan: process intervals by start point. A register frees when its
    // interval ends strictly before the next one begins. Ties favor the most-referenced
    // (then name), so hot variables win a scarce register.
    cands.sort_by(|x, y| {
        x.start
            .cmp(&y.start)
            .then_with(|| y.refs.cmp(&x.refs))
            .then_with(|| x.name.cmp(&y.name))
    });
    // Free pools held descending so `pop()` hands out the lowest-numbered register.
    let mut int_free: Vec<u32> = (19..=28).rev().collect();
    let mut f_free: Vec<u32> = (8..=15).rev().collect();
    let mut active: Vec<(u32, u32, bool, u32, String)> = Vec::new(); // (end, reg, fp, refs, name)
    let mut promote = HashMap::new();
    for c in &cands {
        // Expire intervals that ended before this one starts, freeing registers.
        let mut i = 0;
        while i < active.len() {
            if active[i].0 < c.start {
                let (_, reg, fp, _, _) = active.remove(i);
                let pool = if fp { &mut f_free } else { &mut int_free };
                pool.push(reg);
                pool.sort_unstable_by(|x, y| y.cmp(x));
            } else {
                i += 1;
            }
        }
        let pool = if c.fp { &mut f_free } else { &mut int_free };
        if let Some(reg) = pool.pop() {
            promote.insert(c.name.clone(), reg);
            active.push((c.end, reg, c.fp, c.refs, c.name.clone()));
            continue;
        }
        // Pool full: spill the coldest active interval of this register file if it is
        // strictly colder than `c`, handing its register to the hotter variable. This
        // is a whole-range swap; the evicted one falls back to a frame slot. It is the
        // standard linear-scan spill, ranked by the same loop-weighted hotness as
        // promotion rather than by interval end.
        let victim = active
            .iter()
            .enumerate()
            .filter(|(_, a)| a.2 == c.fp)
            .min_by_key(|(_, a)| a.3)
            .map(|(idx, a)| (idx, a.3, a.1, a.4.clone()));
        if let Some((idx, vrefs, reg, vname)) = victim {
            if vrefs < c.refs {
                promote.remove(&vname);
                promote.insert(c.name.clone(), reg);
                active[idx] = (c.end, reg, c.fp, c.refs, c.name.clone());
            }
            // else: `c` is no hotter than any active interval, so leave it in a slot.
        }
    }
    promote
}

/// A promoted register number identifies its file by range: d8..d15 (the
/// callee-saved double registers) for F64 locals, x19..x28 for integers. The two
/// pools never overlap, so `r < 16` means "FP register".
fn is_fp_reg(r: u32) -> bool {
    r < 16
}

fn is_signed(ty: &Type) -> bool {
    matches!(ty, Type::I8 | Type::I16 | Type::I32 | Type::I64)
}
fn is_unsigned_int(ty: &Type) -> bool {
    matches!(ty, Type::U8 | Type::U16 | Type::U32 | Type::U64)
}

/// The `cset` condition for a comparison `BinOp` with the given operand
/// signedness. Eq/Ne are signedness-independent.
fn cmp_cond(op: BinOp, signed: bool) -> u32 {
    use BinOp::*;
    match op {
        Eq => COND_EQ,
        Ne => COND_NE,
        Lt => is_signed_or(signed, COND_LT, COND_LO),
        Gt => is_signed_or(signed, COND_GT, COND_HI),
        Le => is_signed_or(signed, COND_LE, COND_LS),
        Ge => is_signed_or(signed, COND_GE, COND_HS),
        _ => unreachable!("not a comparison op"),
    }
}
fn is_signed_or(signed: bool, s: u32, u: u32) -> u32 {
    if signed { s } else { u }
}

/// Encode `v` as an `add`/`sub` 12-bit immediate. Returns `(is_sub, imm)` where
/// `imm` is in `0..4096`, or `None` if `v` doesn't fit (the caller then uses a
/// register).
fn add_sub_imm12(v: i64) -> Option<(bool, u32)> {
    if (0..4096).contains(&v) {
        Some((false, v as u32))
    } else if (-4095..0).contains(&v) {
        Some((true, (-v) as u32))
    } else {
        None
    }
}

/// `log2(v)` when `v` is a power of two greater than 1 (`2..2^63`), for strength
/// reduction of `* / %` by a power of two; `None` otherwise.
fn log2_pow2(v: i64) -> Option<u32> {
    if v > 1 && (v & (v - 1)) == 0 {
        Some(v.trailing_zeros())
    } else {
        None
    }
}
fn named_of(ty: &Type, pos: Pos) -> Result<String, CodegenError> {
    match ty {
        Type::Named(n) => Ok(n.clone()),
        _ => Err(CodegenError::at(
            pos,
            "member access on a value that is not a class or union",
        )),
    }
}
trait TypeExt {
    fn elem(&self) -> Option<Type>;
    fn deref_ptr(&self) -> Type;
}
impl TypeExt for Type {
    fn elem(&self) -> Option<Type> {
        match self {
            Type::Ptr(inner) => Some((**inner).clone()),
            Type::Array(inner, _) => Some((**inner).clone()),
            _ => None,
        }
    }
    fn deref_ptr(&self) -> Type {
        match self {
            Type::Ptr(inner) => (**inner).clone(),
            other => other.clone(),
        }
    }
}

fn collect_labels(s: &Stmt, cg: &mut Cg) {
    match &s.kind {
        StmtKind::Label(name) => {
            let id = cg.asm.new_label();
            cg.labels.insert(name.clone(), id);
        }
        StmtKind::Block(b) => b.iter().for_each(|st| collect_labels(st, cg)),
        StmtKind::If { then, else_, .. } => {
            collect_labels(then, cg);
            if let Some(e) = else_ {
                collect_labels(e, cg);
            }
        }
        StmtKind::While { body, .. }
        | StmtKind::DoWhile { body, .. }
        | StmtKind::For { body, .. }
        | StmtKind::Switch { body, .. } => collect_labels(body, cg),
        _ => {}
    }
}

fn compound_binop(op: AssignOp) -> BinOp {
    match op {
        AssignOp::Add => BinOp::Add,
        AssignOp::Sub => BinOp::Sub,
        AssignOp::Mul => BinOp::Mul,
        AssignOp::Div => BinOp::Div,
        AssignOp::Mod => BinOp::Mod,
        AssignOp::BitAnd => BinOp::BitAnd,
        AssignOp::BitOr => BinOp::BitOr,
        AssignOp::BitXor => BinOp::BitXor,
        AssignOp::Shl => BinOp::Shl,
        AssignOp::Shr => BinOp::Shr,
        AssignOp::Assign => unreachable!(),
    }
}

/// Fold a `case` label expression to a constant `i64`, if it is one. Mirrors how
/// `gen_expr` would evaluate these literal forms, so the branch table dispatches
/// identically to the compare-chain. Returns `None` for anything non-constant
/// (the caller then keeps the compare-chain).
fn const_eval_i64(e: &Expr) -> Option<i64> {
    match &e.kind {
        ExprKind::Int(n) | ExprKind::Char(n) => Some(*n),
        ExprKind::Unary { op, expr } => {
            let v = const_eval_i64(expr)?;
            match op {
                UnOp::Neg => Some(v.wrapping_neg()),
                UnOp::Pos => Some(v),
                UnOp::BitNot => Some(!v),
                UnOp::Not => Some(i64::from(v == 0)),
                _ => None,
            }
        }
        ExprKind::Binary { op, lhs, rhs } => {
            let a = const_eval_i64(lhs)?;
            let b = const_eval_i64(rhs)?;
            // `/ % >>` depend on the left operand's signedness, exactly as codegen
            // does, so the folded value matches what the dispatch would compute.
            let signed = lhs.ty().as_ref().is_none_or(is_signed);
            match op {
                BinOp::Add => Some(a.wrapping_add(b)),
                BinOp::Sub => Some(a.wrapping_sub(b)),
                BinOp::Mul => Some(a.wrapping_mul(b)),
                BinOp::Div if b == 0 => None,
                BinOp::Div if signed => a.checked_div(b), // None on MIN/-1 -> fall back
                BinOp::Div => Some(((a as u64) / (b as u64)) as i64),
                BinOp::Mod if b == 0 => None,
                BinOp::Mod if signed => a.checked_rem(b),
                BinOp::Mod => Some(((a as u64) % (b as u64)) as i64),
                BinOp::BitAnd => Some(a & b),
                BinOp::BitOr => Some(a | b),
                BinOp::BitXor => Some(a ^ b),
                BinOp::Shl => Some(a.wrapping_shl(b as u32)),
                BinOp::Shr if signed => Some(a.wrapping_shr(b as u32)),
                BinOp::Shr => Some((a as u64).wrapping_shr(b as u32) as i64),
                // Comparisons / logical ops are rare as case labels; leaving them
                // unfolded just keeps such switches on the compare-chain.
                _ => None,
            }
        }
        _ => None,
    }
}

fn align16(n: u32) -> u32 {
    (n + 15) & !15
}

/// The libc symbol the hosted (Darwin) arm64 backend lowers a heap primitive to.
/// `MAlloc`/`Free` are the one libc-backed group on this target. The freestanding
/// path emits an `mmap` bump allocator instead, using this only as the gate that
/// recognizes the two names. Every other primitive intrinsic is an emitted routine or
/// syscall, so it isn't here.
fn libc_symbol(name: &str) -> Option<&'static str> {
    match name {
        "MAlloc" => Some("_malloc"),
        "Free" => Some("_free"),
        _ => None,
    }
}
