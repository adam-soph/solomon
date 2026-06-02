//! A code-generation backend for Apple-silicon macOS (`aarch64-apple-darwin`).
//!
//! It lowers the program to **hand-emitted AArch64 machine code**, writes a
//! Mach-O relocatable object, and links it with the system `cc`. No
//! LLVM/Cranelift/C — the instruction bytes and the object container are
//! produced here. The [interpreter](crate::interp) is the conformance
//! oracle this backend matches byte-for-byte (see `tests/arm64.rs`).
//!
//! ## Scope
//!
//! Codegen is **type-directed**: it consults the typed AST (`Expr::ty`) and the
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
//!   * **global variables** (Mach-O common symbols addressed via
//!     `PAGE21`/`PAGEOFF12` relocations) and the built-in library (lowered to
//!     libc externs via `BRANCH26` relocations).
//!
//! Several optimizations run during/after emission: constant folding,
//! immediate-form arithmetic, a dead-`mov` peephole, and a linear-scan register
//! allocator that promotes hot locals to callee-saved registers (see
//! `plan_registers` / `Asm::peephole`).
//!
//! Frame: `stp x29,x30,[sp,#-16]!; mov x29,sp; sub sp,sp,#frame`. Locals live
//! below the frame pointer and are addressed as `x29 - offset`; promoted callee-
//! saved registers are spilled into the same frame (`stur`/`fstur`, restored in
//! the epilogue). The epilogue (`mov sp,x29; ldp x29,x30,[sp],#16; ret`) needs no
//! frame size, and only the one `sub sp` immediate is back-patched. Expression
//! evaluation is a stack machine (intermediates spilled to the machine stack) so
//! values survive calls.

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

const RES: u32 = 9; // integer/pointer expression result
const T2: u32 = 10; // secondary integer temporary
const SCRATCH: u32 = 8; // scratch (e.g. `%` quotient, strides, fp<->gpr conduit)
const FRES: u32 = 16; // F64 expression result (v16, caller-saved)
const FT2: u32 = 17; // secondary F64 temporary (v17)
const FP: u32 = 29;
const LR: u32 = 30;
const SP: u32 = 31;

// printf flag bits for the freestanding formatter (mirror the x86-64 backend).
const F_SIGNED: i64 = 1; // a signed conversion (`%d`/`%i`)
const F_UPPER: i64 = 2; // uppercase hex (`%X`) / `0X`
const F_MINUS: i64 = 4; // left-justify
const F_ZERO: i64 = 8; // zero-pad
const F_PLUS: i64 = 16; // always show a sign
const F_SPACE: i64 = 32; // space before a non-negative
const F_HASH: i64 = 64; // alternate form (`0x`/leading `0`)
const NLIMBS: i64 = 48; // limbs in the float-formatting BIGNUM (matches x86-64)

// Freestanding printf scratch buffer sizes, large enough for the clamped
// `fmt::MAX_WIDTH`/`MAX_PRECISION` so the formatters can never overflow them.
const FS_INT_DIGBUF: u64 = 600; // integer digit string (≥ MAX_PRECISION + sign/prefix)
const FS_OUTBUF: u64 = 1152; // assembled field (≥ MAX_WIDTH)
const FS_FLOAT_DIGBUF: u64 = 1024; // `%f` digits (J ≤ ~925, the bignum's capacity)
const FS_SIGBUF: u64 = 576; // `%e`/`%g` significant digits (≥ MAX_PRECISION + 1)
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

// Per-instruction register-liveness tags, used by the peephole pass (`Asm`).
// `inst_use` is a bitmask over the general-purpose registers x0–x30 (bit r = xr);
// x31 (SP/XZR) is never tracked. `inst_branch` classifies control flow.
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

/// The per-OS object-format and link policy. The AArch64 instruction encoding
/// and the code generation are shared between targets; this is the only
/// Darwin-vs-Linux difference — the relocatable-object container (Mach-O vs ELF,
/// with their relocation types and symbol-name conventions) and the linker.
trait ArmTarget {
    /// Package the machine code + symbolic relocations into a relocatable object.
    /// `defined` are the `_main` + function symbols (with their `__text` byte
    /// offsets), `commons` the BSS-allocated globals, `ndefined` the count of
    /// defined symbols. Only hosted targets (Darwin) implement this; a
    /// [`freestanding`](ArmTarget::freestanding) target emits an executable
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
    /// targets (Darwin, via `cc`) implement this; freestanding targets need no
    /// linker.
    fn link(&self, _obj: &Path, _out: &Path) -> Result<(), CodegenError> {
        unreachable!("link is only called for hosted (non-freestanding) targets")
    }

    /// Whether variadic arguments (to `printf`/`sprintf`/…) are passed in
    /// registers (standard AAPCS64 — `true`) or all on the stack (Apple's ARM64
    /// ABI — `false`). The only codegen difference between the two AArch64 OSes.
    fn variadic_in_registers(&self) -> bool;

    /// `true` for a **freestanding** target — one that emits a self-contained
    /// static executable with its own `_start` and raw syscalls, calling no libc
    /// and needing no linker (`aarch64-unknown-linux` with no C toolchain). When
    /// set, the driver emits a `_start` entry and `compile` returns the finished
    /// executable from [`write_executable`](ArmTarget::write_executable) instead of
    /// a relocatable object. The hosted Darwin target leaves this `false` and uses
    /// [`write_object`](ArmTarget::write_object) + `link` (via `cc`).
    fn freestanding(&self) -> bool {
        false
    }

    /// Wrap the freestanding `code` (entry at its first byte, BSS of `bss` zero
    /// bytes trailing the image) into a runnable executable. Only called when
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

    /// Emit the Mach-O relocatable object for `program` as raw bytes (no link).
    /// Exposed so structural tests can byte-check the emitted object on any host.
    pub fn object(&self, program: &Program) -> Result<Vec<u8>, CodegenError> {
        compile(program, &darwin::Darwin)
    }
}

/// Emit a relocatable object for `program` using `target`'s container format.
/// This driver — function/global symbol layout, code emission, fixup resolution
/// — is shared by every AArch64 target; only `target` differs.
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
    // A hidden global word backs `RandU64`'s PRNG state (zero-initialised by
    // the linker; splitmix64 runs from any seed).
    {
        let sym = ndefined + cg.global_order.len() as u32;
        cg.globals.insert(
            crate::builtins::RNG_STATE_GLOBAL.to_string(),
            GlobalInfo { sym, ty: Type::U64 },
        );
        cg.global_order
            .push(crate::builtins::RNG_STATE_GLOBAL.to_string());
    }
    // When the program reads command-line args, reserve two hidden common
    // symbols (argc and the argv array pointer) that `_main` populates from
    // x0/x1; arg-free programs are left untouched.
    if crate::ast::program_calls_any(program, &["ArgC", "ArgV"]) {
        cg.uses_args = true;
        for name in [ARGC_GLOBAL, ARGV_GLOBAL] {
            let sym = ndefined + cg.global_order.len() as u32;
            cg.globals
                .insert(name.to_string(), GlobalInfo { sym, ty: Type::U64 });
            cg.global_order.push(name.to_string());
        }
    }

    // Freestanding: lay out the globals in BSS (no linker to allocate commons), in
    // declaration order with natural alignment, so `addr_global` can address each
    // by a fixed offset.
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

    // Freestanding: emit `_start` (the ELF entry, first byte of `__text`) — call
    // `_main`, then exit_group with its return value. Hosted targets let the libc
    // start-up code call `_main` and turn its return into the exit status.
    if cg.freestanding {
        cg.asm.place(start_label);
        cg.asm.bl(main_label); // x0 = Main()
        cg.asm.load_imm(8, 94); // x8 = SYS_exit_group
        cg.asm.svc(); // exit(x0)
    }

    let driver: Vec<&Stmt> = program
        .items
        .iter()
        .filter(|s| !matches!(s.kind, StmtKind::Func(_) | StmtKind::Class(_)))
        .collect();
    cg.emit_function(main_label, &[], &Type::I64, &driver, true)?;

    for item in &program.items {
        if let StmtKind::Func(f) = &item.kind {
            if let Some(body) = &f.body {
                let label = cg.funcs[&f.name].label;
                let body_refs: Vec<&Stmt> = body.iter().collect();
                cg.emit_function(label, &f.params, &f.ret, &body_refs, false)?;
            }
        }
    }

    // Freestanding runtime routines used by the program (emitted once, at the end
    // of `__text`, so their forward `bl` references resolve in `finish`).
    if cg.freestanding {
        cg.emit_fs_runtime();
    }

    // Freestanding: no symbol table or linker — the image is the executable. For
    // now (no globals/strings/libc lowered yet) any leftover relocation means the
    // program uses a feature not ported to the freestanding backend.
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

    // Symbol table: defined (`_main` + funcs, in __text) then common globals.
    let mut defined = vec![("_main".to_string(), cg.asm.label_byte(main_label)?)];
    for item in &program.items {
        if let StmtKind::Func(f) = &item.kind {
            if f.body.is_some() {
                let off = cg.asm.label_byte(cg.funcs[&f.name].label)?;
                defined.push((format!("_{}", f.name), off));
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
    // Hand the machine code + symbolic relocations to the target's object
    // writer, which lowers the relocations and packages the relocatable object.
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
}

/// A local variable's location. Normally the value lives in the frame at
/// `x29 - off`. For an array parameter (which decays to a pointer, C-style),
/// `indirect` is set: the slot at `x29 - off` holds a *pointer* to the data. A
/// `reg`-promoted local (a non-address-taken scalar — see `plan_registers`)
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
    /// Bytes of frame used below x29 so far (cumulative, monotonic).
    depth: u32,
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
    /// Callee-saved registers used by the current function and where their
    /// incoming value is spilled, as `(reg, frame_off)` — saved in the prologue
    /// and restored in every epilogue.
    cs_saves: Vec<(u32, u32)>,
    /// Whether the program calls `ArgC`/`ArgV`. When set, the `_main` entry
    /// captures the incoming argc/argv (x0/x1) into the hidden globals below.
    uses_args: bool,
    /// Whether variadic args go in registers (AAPCS64) vs the stack (Apple).
    variadic_regs: bool,
    /// Whether this is a freestanding target (own `_start`, raw syscalls, no
    /// libc). Selects emitted runtime over libc calls throughout codegen.
    freestanding: bool,
    /// Freestanding print scratch (allocated once): BSS offsets of `digbuf`
    /// (digit string), `outbuf` (the padded field), `charbuf` (`%c`), and `out_ptr`
    /// (the sink: 0 = stdout, else the current `StrPrint` write cursor).
    fs_scratch_off: Option<[u64; 4]>,
    /// Float-formatter BSS: `(BIGNUM offset, digit-buffer offset)`, allocated once.
    fs_float_off: Option<(u64, u64)>,
    /// `%e`/`%g` scratch BSS: `(egdig, sigbuf, bodybuf)`, allocated once.
    fs_eg_off: Option<(u64, u64, u64)>,
    /// Freestanding BSS layout: each global symbol's byte offset within the BSS
    /// region that follows code+strings, and the running total size. Globals are
    /// addressed by a self-resolved `ADR` to `text_end + offset` (no relocations);
    /// runtime scratch (allocator state, …) is bump-allocated here too.
    global_bss: HashMap<u32, u64>,
    bss_size: u64,
    /// Freestanding builtin runtime routines (name -> label), emitted once at the
    /// end of `__text` in place of the libc calls the hosted backends make.
    fs_routines: HashMap<&'static str, usize>,
}

/// Hidden globals holding the command line, populated at the entry (only when the
/// program uses `ArgC`/`ArgV`). Common symbols, like the RNG state word.
const ARGC_GLOBAL: &str = "__solomon_holyc_argc";
const ARGV_GLOBAL: &str = "__solomon_holyc_argv";

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
            break_targets: Vec::new(),
            continue_targets: Vec::new(),
            labels: HashMap::new(),
            cur_ret: Type::I64,
            sret_off: None,
            promote: HashMap::new(),
            cs_saves: Vec::new(),
            uses_args: false,
            variadic_regs: false,
            freestanding: false,
            fs_scratch_off: None,
            fs_float_off: None,
            fs_eg_off: None,
            global_bss: HashMap::new(),
            bss_size: 0,
            fs_routines: HashMap::new(),
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
    /// its first byte (relative to the BSS base = end of code+strings).
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
    ) -> Result<(), CodegenError> {
        self.scopes = vec![HashMap::new()];
        self.depth = 0;
        self.break_targets.clear();
        self.continue_targets.clear();
        self.labels.clear();
        self.cur_ret = ret.clone();
        self.sret_off = None;
        self.promote = plan_registers(params, body);
        self.cs_saves.clear();

        for s in body {
            collect_labels(s, self);
        }

        self.asm.place(entry);
        self.asm.stp_pre_fp_lr(); // stp x29,x30,[sp,#-16]!
        self.asm.mov_fp_sp(); // x29 = sp
        let sub_idx = self.asm.emit_sub_sp_placeholder();

        // Spill the incoming value of every callee-saved register we'll reuse for
        // a promoted local, near x29 (one STUR each). Restored in every epilogue.
        // Distinct registers only — with live-range sharing several locals may map
        // to the same register, but it is saved/restored once.
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

        let locals = align16(self.depth);
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
                            self.gen_init_into(&Place::Local(off), &d.ty, 0, init)?;
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
                        // interpreter (the conformance oracle) — without this, its
                        // elements/fields would read back as stack garbage.
                        None => self.gen_zero_slot(off, size),
                    }
                }
            }

            StmtKind::If { cond, then, else_ } => {
                self.gen_expr(cond)?;
                let l_else = self.asm.new_label();
                self.asm.cbz(RES, l_else);
                self.gen_stmt(then)?;
                if let Some(else_branch) = else_ {
                    let l_end = self.asm.new_label();
                    self.asm.b(l_end);
                    self.asm.place(l_else);
                    self.gen_stmt(else_branch)?;
                    self.asm.place(l_end);
                } else {
                    self.asm.place(l_else);
                }
            }

            StmtKind::While { cond, body } => {
                let l_top = self.asm.new_label();
                let l_end = self.asm.new_label();
                self.asm.place(l_top);
                self.gen_expr(cond)?;
                self.asm.cbz(RES, l_end);
                self.break_targets.push(l_end);
                self.continue_targets.push(l_top);
                self.gen_stmt(body)?;
                self.break_targets.pop();
                self.continue_targets.pop();
                self.asm.b(l_top);
                self.asm.place(l_end);
            }

            StmtKind::DoWhile { body, cond } => {
                let l_top = self.asm.new_label();
                let l_cont = self.asm.new_label();
                let l_end = self.asm.new_label();
                self.asm.place(l_top);
                self.break_targets.push(l_end);
                self.continue_targets.push(l_cont);
                self.gen_stmt(body)?;
                self.break_targets.pop();
                self.continue_targets.pop();
                self.asm.place(l_cont);
                self.gen_expr(cond)?;
                self.asm.cbnz(RES, l_top);
                self.asm.place(l_end);
            }

            StmtKind::For {
                init,
                cond,
                step,
                body,
            } => {
                self.scopes.push(HashMap::new());
                if let Some(init) = init {
                    self.gen_stmt(init)?;
                }
                let l_top = self.asm.new_label();
                let l_cont = self.asm.new_label();
                let l_end = self.asm.new_label();
                self.asm.place(l_top);
                if let Some(cond) = cond {
                    self.gen_expr(cond)?;
                    self.asm.cbz(RES, l_end);
                }
                self.break_targets.push(l_end);
                self.continue_targets.push(l_cont);
                self.gen_stmt(body)?;
                self.break_targets.pop();
                self.continue_targets.pop();
                self.asm.place(l_cont);
                if let Some(step) = step {
                    self.gen_expr(step)?;
                }
                self.asm.b(l_top);
                self.asm.place(l_end);
                self.scopes.pop();
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
                        // Copy the aggregate through the saved sret pointer.
                        self.gen_expr(e)?; // RES = source address
                        let off = self.sret_off.expect("aggregate return needs sret slot");
                        self.asm.sub_imm(T2, FP, off);
                        self.asm.load_mem(T2, T2, 8, false); // T2 = sret pointer
                        let n = self.type_size(&self.cur_ret);
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

            StmtKind::Func(_) | StmtKind::Class(_) => {
                return Err(CodegenError::at(
                    s.span.pos,
                    "arm64 backend: nested functions/classes are not supported",
                ));
            }
        }
        Ok(())
    }

    fn gen_switch(&mut self, cond: &Expr, body: &Stmt, pos: Pos) -> Result<(), CodegenError> {
        let StmtKind::Block(stmts) = &body.kind else {
            return Err(CodegenError::at(pos, "switch body must be a block"));
        };

        self.gen_expr(cond)?;
        let voff = self.alloc(8, 8);
        self.asm.sub_imm(T2, FP, voff);
        self.gen_store(RES, T2, &Type::I64);

        // HolyC `start:` / `end:` sub-labels partition the body into an optional
        // prologue (runs on entry, before dispatch) and epilogue (reached by
        // fall-through; `break` skips it). Sema has checked the ordering.
        let start_idx = stmts
            .iter()
            .position(|s| matches!(s.kind, StmtKind::SwitchStart));
        let first_case = stmts
            .iter()
            .position(|s| matches!(s.kind, StmtKind::Case { .. } | StmtKind::Default));
        let end_idx = stmts
            .iter()
            .position(|s| matches!(s.kind, StmtKind::SwitchEnd));
        let prologue = start_idx.map(|si| (si + 1)..first_case.unwrap_or(stmts.len()));

        let l_end = self.asm.new_label();
        self.break_targets.push(l_end);
        self.scopes.push(HashMap::new());

        // Prologue: always runs, before the dispatch compares.
        if let Some(range) = prologue.clone() {
            for st in &stmts[range] {
                self.gen_stmt(st)?;
            }
        }

        let mut label_at: HashMap<usize, usize> = HashMap::new();
        let mut default_label: Option<usize> = None;
        let end_label = end_idx.map(|_| self.asm.new_label());
        for (i, st) in stmts.iter().enumerate() {
            match &st.kind {
                StmtKind::Case { .. } => {
                    label_at.insert(i, self.asm.new_label());
                }
                StmtKind::Default => {
                    let l = self.asm.new_label();
                    label_at.insert(i, l);
                    default_label = Some(l);
                }
                _ => {}
            }
        }

        // No case matched: fall to default, else the epilogue, else the exit.
        let gap_target = default_label.or(end_label).unwrap_or(l_end);
        // Prefer an O(1) branch table when the cases are dense integer constants;
        // otherwise fall back to a linear compare-chain.
        if !self.try_gen_branch_table(stmts, &label_at, voff, gap_target)? {
            for (i, st) in stmts.iter().enumerate() {
                if let StmtKind::Case { lo, hi } = &st.kind {
                    let target = label_at[&i];
                    self.gen_expr(lo)?;
                    self.asm.mov_reg(T2, RES);
                    self.load_local(RES, voff, &Type::I64);
                    self.asm.cmp_reg(RES, T2);
                    match hi {
                        None => self.asm.b_cond(COND_EQ, target),
                        Some(hi) => {
                            let skip = self.asm.new_label();
                            self.asm.b_cond(COND_LT, skip);
                            self.gen_expr(hi)?;
                            self.asm.mov_reg(T2, RES);
                            self.load_local(RES, voff, &Type::I64);
                            self.asm.cmp_reg(RES, T2);
                            self.asm.b_cond(COND_GT, skip);
                            self.asm.b(target);
                            self.asm.place(skip);
                        }
                    }
                }
            }
            self.asm.b(gap_target);
        }

        for (i, st) in stmts.iter().enumerate() {
            if prologue.as_ref().is_some_and(|r| r.contains(&i)) {
                continue; // already emitted as the prologue
            }
            if let Some(&l) = label_at.get(&i) {
                self.asm.place(l);
            }
            match &st.kind {
                StmtKind::Case { .. } | StmtKind::Default | StmtKind::SwitchStart => {}
                StmtKind::SwitchEnd => {
                    if let Some(l) = end_label {
                        self.asm.place(l);
                    }
                }
                _ => self.gen_stmt(st)?,
            }
        }
        self.scopes.pop();
        self.break_targets.pop();
        self.asm.place(l_end);
        Ok(())
    }

    /// Try to dispatch a switch through an O(1) jump table instead of a linear
    /// compare-chain. Returns `Ok(true)` when it emitted the table (the caller
    /// then skips the compare-chain), `Ok(false)` to fall back.
    ///
    /// Fires only when every `case` value is a compile-time integer constant and
    /// the covered value span is small/dense enough to be worth a table. The
    /// table is `span` 32-bit offset words (`table[k] = label_k - table`);
    /// dispatch is `idx = v - min`, an unsigned bounds check, then
    /// `LDRSW off, [table, idx, lsl #2]; BR (table + off)`. Out-of-range and gap
    /// values go to `gap_target` (the switch's default / epilogue / exit), and
    /// overlapping ranges resolve to the first covering case — both matching the
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
        self.asm.br(T2); // unconditional — the table data below is never run as code
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
            self.gen_init_into(&Place::Global(sym), &ty, 0, init)?;
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

    /// Emit the stores for a brace initialiser (or a single leaf value) into the
    /// aggregate at `place`, at byte offset `byte_off`. Recurses for nested
    /// arrays/structs; only provided elements are written (locals are zeroed
    /// first, globals are linker-zeroed).
    fn gen_init_into(
        &mut self,
        place: &Place,
        ty: &Type,
        byte_off: u32,
        init: &Expr,
    ) -> Result<(), CodegenError> {
        if let ExprKind::InitList(items) = &init.kind {
            match ty {
                Type::Array(elem, _) => {
                    let stride = self.layouts.stride_of(elem) as u32;
                    for (i, item) in items.iter().enumerate() {
                        self.gen_init_into(place, elem, byte_off + i as u32 * stride, item)?;
                    }
                }
                Type::Named(class) => {
                    let fields: Vec<(Type, u32)> = self
                        .layouts
                        .get(class)
                        .map(|l| {
                            l.fields
                                .iter()
                                .map(|f| (f.ty.clone(), f.offset as u32))
                                .collect()
                        })
                        .unwrap_or_default();
                    for (item, (fty, foff)) in items.iter().zip(fields.iter()) {
                        self.gen_init_into(place, fty, byte_off + foff, item)?;
                    }
                }
                _ => {
                    return Err(CodegenError::at(
                        init.span.pos,
                        "arm64 backend: an initializer list can only initialize an array, class, or union",
                    ));
                }
            }
            return Ok(());
        }
        if let ExprKind::DesignatedInit(items) = &init.kind {
            let Type::Named(class) = ty else {
                return Err(CodegenError::at(
                    init.span.pos,
                    "arm64 backend: a designated initializer can only initialize a class or union",
                ));
            };
            // Field name -> (type, offset), captured before the store loop.
            let fields: Vec<(String, Type, u32)> = self
                .layouts
                .get(class)
                .map(|l| {
                    l.fields
                        .iter()
                        .map(|f| (f.name.clone(), f.ty.clone(), f.offset as u32))
                        .collect()
                })
                .unwrap_or_default();
            for (name, value) in items {
                let Some((_, fty, foff)) = fields.iter().find(|(n, _, _)| n == name) else {
                    return Err(CodegenError::at(
                        value.span.pos,
                        format!("arm64 backend: `{class}` has no field `{name}`"),
                    ));
                };
                self.gen_init_into(place, &fty.clone(), byte_off + foff, value)?;
            }
            return Ok(());
        }
        // A leaf value: scalar, pointer, float, or an aggregate-valued expression.
        if is_f64(ty) {
            self.gen_foperand(init)?;
            self.elem_addr(T2, place, byte_off);
            self.asm.fmov_to_gpr(RES, FRES);
            self.asm.store_mem(RES, T2, 8);
        } else if is_aggregate(ty) {
            self.gen_expr(init)?; // RES = source address
            self.elem_addr(T2, place, byte_off);
            self.gen_memcpy(T2, RES, self.type_size(ty), SCRATCH);
        } else {
            self.gen_int_expr(init, ty)?;
            self.elem_addr(T2, place, byte_off);
            self.gen_store(RES, T2, ty);
        }
        Ok(())
    }

    // ---- expressions: value -> RES ----

    /// Evaluate `e` to an integer in RES for storage into a `target`-typed slot.
    /// Identical to `gen_expr` except that converting an F64 source to an
    /// **unsigned** integer target uses `fcvtzu` instead of the default `fcvtzs`
    /// (they differ past `I64::MAX` and for negatives) — matching C and the
    /// interpreter's `cast_value`.
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
        // function's contract is "integer/pointer result in RES", so when an
        // F64 value reaches here it is in integer context (assignment to an int
        // slot, an int parameter/return, an int array element, …) and must be
        // truncated to an integer — matching C / the interpreter — rather than
        // having its raw bit pattern stored.
        if is_f64(&self.expr_ty(e)) {
            self.gen_fexpr(e)?;
            self.asm.fcvtzs(RES, FRES);
            return Ok(());
        }
        // Constant-fold a compile-time integer expression to a single `load_imm`.
        // `const_eval_i64` mirrors the runtime arithmetic exactly (and only
        // succeeds for pure-integer operand trees), so this can't change behavior.
        if matches!(&e.kind, ExprKind::Binary { .. } | ExprKind::Unary { .. }) {
            if let Some(n) = const_eval_i64(e) {
                self.asm.load_imm(RES, n);
                return Ok(());
            }
        }
        let pos = e.span.pos;
        match &e.kind {
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

    /// Immediate-form fast paths for `<expr> op <small constant>` (and the
    /// commutative `<const> + <expr>`): emit the operation against an immediate
    /// instead of materializing the constant in a register. Returns whether it
    /// handled the op. (Fully-constant expressions already fold in `gen_expr`.)
    fn try_imm_binop(
        &mut self,
        op: BinOp,
        lhs: &Expr,
        rhs: &Expr,
        lt: &Type,
        rt: &Type,
    ) -> Result<bool, CodegenError> {
        use BinOp::*;
        // Add is commutative — the constant may be on either side.
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
        // Multiplication by a constant power of two is a left shift (commutative,
        // signedness-independent — both wrap mod 2^64).
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
            // Unsigned divide / modulo by a power of two reduce to a logical
            // shift / a low-bits mask. (Signed needs a bias to round toward zero,
            // so it keeps the generic SDIV/MSUB path.)
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
            // (C semantics) — matching the interpreter.
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
        // A name that isn't a registered builtin is an ordinary function call —
        // even if it shares a name with a former builtin now living in the stdlib
        // (`Sign`, `StrToUpper`, `StrRev`, `StrLen`, …). Skip the builtin lowering
        // and call the compiled body.
        if !crate::builtins::is_builtin(name) {
            return self.emit_user_call(name, args, pos);
        }
        // `Sign(x)` = `(x > 0) - (x < 0)` — a computed builtin with no libc
        // counterpart, emitted inline.
        if name == "Sign" {
            self.gen_expr(&args[0])?; // value -> RES
            self.asm.cmp_imm0(RES);
            self.asm.cset(T2, COND_GT); // 1 if positive
            self.asm.cset(RES, COND_LT); // 1 if negative
            self.asm.sub(RES, T2, RES); // (>0) - (<0)
            return Ok(());
        }
        // `RandU64()` — splitmix64 over a hidden global state word, emitted inline
        // so its sequence matches the interpreter's `splitmix64`.
        if name == "RandU64" {
            let sym = self.globals[crate::builtins::RNG_STATE_GLOBAL].sym;
            self.addr_global(T2, sym); // T2 = &state
            self.asm.load_mem(RES, T2, 8, false);
            self.asm.load_imm(SCRATCH, 0x9e3779b97f4a7c15u64 as i64);
            self.asm.add(RES, RES, SCRATCH); // state += golden
            self.asm.store_mem(RES, T2, 8); // write it back; RES holds z
            // z = (z ^ (z >> 30)) * C1
            self.asm.load_imm(SCRATCH, 30);
            self.asm.lsrv(T2, RES, SCRATCH);
            self.asm.eor(RES, RES, T2);
            self.asm.load_imm(SCRATCH, 0xbf58476d1ce4e5b9u64 as i64);
            self.asm.mul(RES, RES, SCRATCH);
            // z = (z ^ (z >> 27)) * C2
            self.asm.load_imm(SCRATCH, 27);
            self.asm.lsrv(T2, RES, SCRATCH);
            self.asm.eor(RES, RES, T2);
            self.asm.load_imm(SCRATCH, 0x94d049bb133111ebu64 as i64);
            self.asm.mul(RES, RES, SCRATCH);
            // z ^= z >> 31
            self.asm.load_imm(SCRATCH, 31);
            self.asm.lsrv(T2, RES, SCRATCH);
            self.asm.eor(RES, RES, T2);
            return Ok(());
        }
        // `ArgC()` / `ArgV(i)` — read the command line captured at the entry.
        if name == "ArgC" {
            let sym = self.globals[ARGC_GLOBAL].sym;
            self.addr_global(T2, sym);
            self.asm.load_mem(RES, T2, 8, false); // RES = argc
            return Ok(());
        }
        if name == "ArgV" {
            self.gen_expr(&args[0])?; // RES = i
            self.asm.mov_reg(T2, RES); // T2 = i (saved across the loads below)
            let l_null = self.asm.new_label();
            let l_done = self.asm.new_label();
            // Out of range (including negative i, via the unsigned compare) -> NULL.
            let csym = self.globals[ARGC_GLOBAL].sym;
            self.addr_global(SCRATCH, csym);
            self.asm.load_mem(SCRATCH, SCRATCH, 8, false); // SCRATCH = argc
            self.asm.cmp_reg(T2, SCRATCH);
            self.asm.b_cond(COND_HS, l_null); // unsigned i >= argc
            // RES = argv[i] = *(argv_base + i*8)
            let vsym = self.globals[ARGV_GLOBAL].sym;
            self.addr_global(RES, vsym);
            self.asm.load_mem(RES, RES, 8, false); // RES = argv base pointer
            self.asm.lsl_imm(SCRATCH, T2, 3); // SCRATCH = i*8
            self.asm.add(RES, RES, SCRATCH);
            self.asm.load_mem(RES, RES, 8, false); // RES = argv[i]
            self.asm.b(l_done);
            self.asm.place(l_null);
            self.asm.load_imm(RES, 0); // NULL
            self.asm.place(l_done);
            return Ok(());
        }
        // `StrToUpper`/`StrToLower` — ASCII-case a string in place via an inline
        // loop calling libc `toupper`/`tolower`; return the string.
        if name == "StrToUpper" || name == "StrToLower" {
            return self.gen_str_case(&args[0], name == "StrToUpper");
        }
        if name == "StrRev" {
            return self.gen_str_rev(&args[0]);
        }
        // The `Is*` ctype predicates — emitted inline as ASCII range checks (libc's
        // `isdigit` etc. return an unspecified nonzero, which wouldn't match the
        // interpreter's 0/1), giving 0 or 1 in RES.
        if let Some(ranges) = crate::builtins::ctype_ranges(name) {
            return self.gen_ctype(&args[0], ranges);
        }
        // Freestanding: lower libc-backed builtins to emitted AArch64 routines (or
        // inline sequences) instead of libc calls.
        if self.freestanding && crate::builtins::libc_symbol(name).is_some() {
            return self.gen_builtin_fs(name, args, pos);
        }
        // A libc-backed builtin (e.g. `StrLen` -> `_strlen`) is an external call;
        // its argument classes come from the inferred call-site types and its
        // return type from the builtin registry.
        if let Some(sym) = crate::builtins::libc_symbol(name) {
            let params: Vec<Param> = args
                .iter()
                .map(|a| Param {
                    ty: self.expr_ty(a),
                    name: None,
                    default: None,
                    span: Span::dummy(),
                })
                .collect();
            let ret = crate::builtins::ret_of(name).unwrap_or(Type::I64);
            self.emit_call(CallTarget::Extern(sym), &params, args, &ret, name, pos)?;
            if name == "StrCmp" || name == "MemCmp" || name == "StrNCmp" {
                // libc compare functions may return any (signed) magnitude; reduce
                // it to a sign in {-1, 0, 1} so the result matches the interpreter.
                self.asm.sxtw(T2, RES);
                self.asm.cmp_imm0(T2);
                self.asm.cset(RES, COND_GT); // 1 if positive
                self.asm.cset(T2, COND_LT); // 1 if negative
                self.asm.sub(RES, RES, T2); // sign
            }
            return Ok(());
        }
        self.emit_user_call(name, args, pos)
    }

    /// Emit a direct call to a user-defined function's compiled body.
    fn emit_user_call(&mut self, name: &str, args: &[Expr], pos: Pos) -> Result<(), CodegenError> {
        let (label, params, ret) = match self.funcs.get(name) {
            Some(info) => (info.label, info.params.clone(), info.ret.clone()),
            None => {
                return Err(CodegenError::at(
                    pos,
                    format!("arm64 backend: cannot call `{name}` (no compiled body)"),
                ));
            }
        };
        self.emit_call(CallTarget::Label(label), &params, args, &ret, name, pos)
    }

    /// Whether `name` resolves to a variable (local or global) rather than a
    /// function — i.e. calling it is an indirect (function-pointer) call.
    fn is_variable(&self, name: &str) -> bool {
        self.lookup(name).is_some() || self.globals.contains_key(name)
    }

    /// Whether evaluating `e` provably touches only RES (never the lhs temp T2),
    /// so a binary op can keep its lhs in T2 rather than spilling to the stack.
    /// Literals, constant-folded subtrees, and scalar variables qualify; anything
    /// that recurses through T2 / the stack (nested binops, calls, indexing) does
    /// not — `gen_addr_ident` and `load_imm` both work only through RES.
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
    /// operand touches only FRES (never the lhs temp FT2). A float literal, or any
    /// integer/scalar `is_simple_operand` (which converts to a double through
    /// RES/FRES), qualifies — so an F64 binary op can keep its lhs in FT2 rather
    /// than spilling it through a GPR and the machine stack.
    fn is_simple_foperand(&self, e: &Expr) -> bool {
        matches!(&e.kind, ExprKind::Float(_)) || self.is_simple_operand(e)
    }

    /// Dispatch a call expression: a bare function/builtin name is a direct call;
    /// anything else (a function-pointer variable or computed value) is indirect.
    fn gen_call_expr(&mut self, callee: &Expr, args: &[Expr]) -> Result<(), CodegenError> {
        let pos = callee.span.pos;
        if let ExprKind::Ident(name) = &callee.kind {
            // Builtins with bespoke lowering. Gated on `is_builtin` so a user
            // function sharing one of these names (e.g. a stdlib `I64ToStr`) is an
            // ordinary call, not a mis-lowered builtin.
            if crate::builtins::is_builtin(name) {
                match name.as_str() {
                    "Print" => return self.gen_print_call(args, pos),
                    "StrPrint" => return self.gen_formatted_write(args, pos, false),
                    "CatPrint" => return self.gen_formatted_write(args, pos, true),
                    "MStrPrint" => return self.gen_mstrprint(args, pos),
                    "F64ToStr" => return self.gen_tostr(&args[0], &args[1], "%g", true, pos),
                    _ => return self.gen_call(name, args, pos),
                }
            }
            if !self.is_variable(name) {
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
    ) -> Result<(), CodegenError> {
        let n = params.len();

        // For an indirect call, evaluate the function-pointer value up front and
        // spill it on the stack so it survives argument evaluation (it is popped
        // back just before the `blr`, after the arg pushes/pops are balanced).
        if let CallTarget::Indirect(callee) = target {
            self.gen_expr(callee)?; // RES = function address
            self.asm.push(RES);
        }

        // A by-value aggregate result is returned through a caller-allocated
        // temporary whose address is handed to the callee in x8.
        let sret_off = if is_aggregate(ret) {
            let size = self.type_size(ret).max(1);
            let align = self.type_align(ret);
            Some(self.alloc(size, align))
        } else {
            None
        };

        // Evaluate each argument left-to-right, spilling its raw 8 bytes (an
        // integer/pointer or class address, or the bit pattern of a double).
        for i in 0..n {
            let arg = if i < args.len() {
                &args[i]
            } else {
                params[i].default.as_ref().ok_or_else(|| {
                    CodegenError::at(pos, format!("missing argument for `{name}`"))
                })?
            };
            if is_f64(&params[i].ty) {
                self.gen_foperand(arg)?;
                self.asm.fmov_to_gpr(RES, FRES);
            } else {
                self.gen_int_expr(arg, &params[i].ty)?;
            }
            self.asm.push(RES);
        }

        // Assign each argument to its ABI register: x0.. for integers, v0.. for
        // doubles, numbered independently.
        let mut igr = 0u32;
        let mut fpr = 0u32;
        let mut targets = Vec::with_capacity(n);
        for p in params {
            if is_f64(&p.ty) {
                if fpr > 7 {
                    return Err(CodegenError::at(
                        pos,
                        "arm64 backend: at most 8 floating-point arguments",
                    ));
                }
                targets.push((true, fpr));
                fpr += 1;
            } else {
                if igr > 7 {
                    return Err(CodegenError::at(
                        pos,
                        "arm64 backend: at most 8 integer arguments",
                    ));
                }
                targets.push((false, igr));
                igr += 1;
            }
        }
        for i in (0..n).rev() {
            let (is_float, reg) = targets[i];
            if is_float {
                self.asm.pop(RES);
                self.asm.fmov_from_gpr(reg, RES);
            } else {
                self.asm.pop(reg);
            }
        }

        if let Some(off) = sret_off {
            self.asm.sub_imm(SCRATCH, FP, off); // x8 = &result temp
        }
        match target {
            CallTarget::Label(label) => self.asm.bl(label),
            CallTarget::Extern(sym) => self.asm.bl_extern(sym),
            CallTarget::Indirect(_) => {
                // The function address was spilled first, so it is on top of the
                // stack now that the arguments have been popped into registers.
                self.asm.pop(T2);
                self.asm.blr(T2);
            }
        }
        if let Some(off) = sret_off {
            self.asm.sub_imm(RES, FP, off); // result value is the temp's address
        } else if is_f64(ret) {
            self.asm.fmov_reg(FRES, 0); // result in d0
        } else {
            self.asm.mov_reg(RES, 0);
        }
        Ok(())
    }

    fn gen_expr_stmt(&mut self, e: &Expr) -> Result<(), CodegenError> {
        match &e.kind {
            ExprKind::Str(s) => self.gen_print(s, &[]),
            ExprKind::Comma(items) => {
                if let Some(first) = items.first() {
                    if let ExprKind::Str(fmt) = &first.kind {
                        let fmt = fmt.clone();
                        return self.gen_print(&fmt, &items[1..]);
                    }
                }
                self.gen_expr(e)
            }
            _ => self.gen_expr(e),
        }
    }

    fn gen_print_call(&mut self, args: &[Expr], pos: Pos) -> Result<(), CodegenError> {
        let (fmt, rest) = match args.split_first() {
            Some((first, rest)) => match &first.kind {
                ExprKind::Str(s) => (s.clone(), rest),
                _ => {
                    return Err(CodegenError::at(
                        pos,
                        "arm64 backend: Print's format must be a string literal",
                    ));
                }
            },
            None => return Err(CodegenError::at(pos, "Print requires a format string")),
        };
        self.gen_print(&fmt, rest)
    }

    /// Spill the variadic args to scratch stack slots `[sp + i*8]` (each int or
    /// double bit-pattern in one 8-byte slot). The slots are how Apple's ARM64 ABI
    /// passes variadics directly; on Linux [`load_variadic_regs`] then copies them
    /// into argument registers.
    fn spill_variadics(&mut self, args: &[Expr], base_off: u32) -> Result<(), CodegenError> {
        for (i, arg) in args.iter().enumerate() {
            if is_f64(&self.expr_ty(arg)) {
                self.gen_fexpr(arg)?;
                self.asm.fmov_to_gpr(RES, FRES);
            } else {
                self.gen_expr(arg)?;
            }
            self.asm.str_sp(RES, base_off + i as u32 * 8);
        }
        Ok(())
    }

    /// AAPCS64 (Linux): load the spilled variadic args into argument registers —
    /// integers into `x{ngrn}..x7`, doubles into `v0..v7`. Beyond the register
    /// capacity they would spill to the stack (unsupported; very rare for printf).
    /// A no-op under Apple's ABI, where the stack slots are the arguments.
    fn load_variadic_regs(&mut self, args: &[Expr], base_off: u32, mut ngrn: u32) {
        if !self.variadic_regs {
            return;
        }
        let mut nsrn = 0u32;
        for (i, arg) in args.iter().enumerate() {
            let off = base_off + i as u32 * 8;
            if is_f64(&self.expr_ty(arg)) {
                if nsrn <= 7 {
                    self.asm.load_mem_off(SCRATCH, SP, off, 8, false);
                    self.asm.fmov_from_gpr(nsrn, SCRATCH);
                }
                nsrn += 1;
            } else {
                if ngrn <= 7 {
                    self.asm.load_mem_off(ngrn, SP, off, 8, false);
                }
                ngrn += 1;
            }
        }
    }

    /// Pass `args` as variadic arguments, with `named_gp` general-purpose argument
    /// registers already taken by named args (so integer variadics start at
    /// `x{named_gp}`). Under Apple's ABI they all go on the stack at `[sp + i*8]`;
    /// under AAPCS64 integers fill `x{named_gp}..x7`, doubles `v0..v7`, and any
    /// that overflow are packed contiguously at `[sp + 0..]`. Returns the stack
    /// space reserved (to add back to `sp` after the call).
    fn pass_variadics(&mut self, args: &[Expr], named_gp: u32) -> Result<u32, CodegenError> {
        if !self.variadic_regs {
            let varsize = align16(args.len() as u32 * 8);
            if varsize > 0 {
                self.asm.sub_sp_imm(varsize);
            }
            self.spill_variadics(args, 0)?;
            return Ok(varsize);
        }
        // Classify each arg: an argument register, or an overflow stack slot.
        let (mut gp, mut fp, mut overflow) = (named_gp, 0u32, 0u32);
        let plan: Vec<(bool, Option<u32>, Option<u32>)> = args
            .iter()
            .map(|a| {
                let f = is_f64(&self.expr_ty(a));
                if f && fp <= 7 {
                    fp += 1;
                    (true, Some(fp - 1), None)
                } else if !f && gp <= 7 {
                    gp += 1;
                    (false, Some(gp - 1), None)
                } else {
                    overflow += 1;
                    (f, None, Some(overflow - 1))
                }
            })
            .collect();
        // Reserve the overflow block (at sp+0) plus scratch for evaluation (above).
        let overflow_size = align16(overflow * 8);
        let total = overflow_size + align16(args.len() as u32 * 8);
        if total > 0 {
            self.asm.sub_sp_imm(total);
        }
        self.spill_variadics(args, overflow_size)?;
        for (i, &(f, reg, ov)) in plan.iter().enumerate() {
            let src = overflow_size + i as u32 * 8;
            if let Some(r) = reg {
                if f {
                    self.asm.load_mem_off(SCRATCH, SP, src, 8, false);
                    self.asm.fmov_from_gpr(r, SCRATCH);
                } else {
                    self.asm.load_mem_off(r, SP, src, 8, false);
                }
            } else if let Some(j) = ov {
                self.asm.load_mem_off(SCRATCH, SP, src, 8, false);
                self.asm.str_sp(SCRATCH, j * 8); // overflow args packed at sp+0..
            }
        }
        Ok(total)
    }

    fn gen_print(&mut self, fmt: &str, args: &[Expr]) -> Result<(), CodegenError> {
        if self.freestanding {
            return self.gen_print_fs(fmt, args);
        }
        let c_fmt = translate_format(fmt)?;
        let fmt_idx = self.asm.intern_string(&c_fmt);
        let sz = self.pass_variadics(args, 1)?; // x0 = fmt, integer variadics from x1
        self.asm.adr(0, fmt_idx);
        self.asm.bl_printf();
        if sz > 0 {
            self.asm.add_sp_imm(sz);
        }
        Ok(())
    }

    /// Freestanding `StrPrint`/`CatPrint`: format into `dst` (or `dst + StrLen(dst)`
    /// for an append) by redirecting the output sink (`out_ptr`) at the buffer, then
    /// NUL-terminating and resetting the sink. Returns `dst`.
    fn gen_formatted_write_fs(
        &mut self,
        dst: &Expr,
        fmt: &str,
        rest: &[Expr],
        append: bool,
    ) -> Result<(), CodegenError> {
        self.fs_routine("OutWrite"); // ensure the sink is emitted
        let out_ptr = self.fs_scratch()[3];
        // dst -> frame slot (survives formatting; also the result).
        self.gen_expr(dst)?;
        let dst_off = self.alloc(8, 8);
        self.asm.sub_imm(T2, FP, dst_off);
        self.gen_store(RES, T2, &Type::I64);
        // Compute the write start (T2): dst, or dst + StrLen(dst) for an append.
        if append {
            self.load_local(0, dst_off, &Type::I64);
            let sl = self.fs_routine("StrLen");
            self.asm.bl(sl); // x0 = StrLen(dst)
            self.load_local(T2, dst_off, &Type::I64);
            self.asm.add(T2, T2, 0); // T2 = dst + len
        } else {
            self.load_local(T2, dst_off, &Type::I64);
        }
        // out_ptr = write start.
        self.asm.adr_global_fs(SCRATCH, out_ptr);
        self.asm.store_mem(T2, SCRATCH, 8);
        // Format through the sink (it appends to the buffer while out_ptr != 0).
        self.gen_print_fs(fmt, rest)?;
        // NUL-terminate at the cursor, then disarm the sink (out_ptr = 0).
        self.asm.adr_global_fs(SCRATCH, out_ptr);
        self.asm.load_mem(T2, SCRATCH, 8, false);
        self.asm.load_imm(RES, 0);
        self.asm.store_mem(RES, T2, 1); // *cursor = '\0'
        self.asm.store_mem(RES, SCRATCH, 8); // out_ptr = 0
        self.load_local(RES, dst_off, &Type::I64); // return dst
        Ok(())
    }

    /// Freestanding `Print`: emit the formatting inline against raw `write`
    /// syscalls instead of calling libc `printf`. Walks the format string (the same
    /// [`crate::fmt`] spec the other backends use); literal runs and each `%`
    /// conversion are rendered and written in turn. Integers go through the shared
    /// [`emit_fmt_int_plain`](Self::emit_fmt_int_plain) routine; `%c`/`%s` are tiny
    /// inline sequences. Flags/width/precision and floats aren't handled yet — an
    /// unsupported spec is a clear error rather than wrong output.
    fn gen_print_fs(&mut self, fmt: &str, args: &[Expr]) -> Result<(), CodegenError> {
        let mut chars = fmt.chars().peekable();
        let mut arg_i = 0usize;
        let mut lit = String::new();
        while let Some(c) = chars.next() {
            if c != '%' {
                lit.push(c);
                continue;
            }
            let spec = crate::fmt::parse(&mut chars);
            if spec.conv == '%' {
                lit.push('%');
                continue;
            }
            self.emit_literal_fs(&lit);
            lit.clear();
            if spec.width_star || spec.prec_star {
                return Err(CodegenError::new(
                    "freestanding aarch64-linux: `*` width/precision not supported yet",
                    None,
                ));
            }
            let i = arg_i;
            if i >= args.len() {
                return Err(CodegenError::new(
                    "freestanding aarch64-linux: too few arguments for format string",
                    None,
                ));
            }
            arg_i += 1;
            let mut flags: i64 = 0;
            if spec.minus {
                flags |= F_MINUS;
            }
            if spec.plus {
                flags |= F_PLUS;
            }
            if spec.space {
                flags |= F_SPACE;
            }
            if spec.zero {
                flags |= F_ZERO;
            }
            if spec.hash {
                flags |= F_HASH;
            }
            let width = spec.width.unwrap_or(0) as i64;
            let prec = if spec.has_precision {
                spec.precision as i64
            } else {
                -1
            };
            self.fs_routine("OutWrite"); // ensure the sink is emitted
            match spec.conv {
                'd' | 'i' | 'u' | 'x' | 'X' | 'o' => {
                    let (radix, extra) = match spec.conv {
                        'd' | 'i' => (10, F_SIGNED),
                        'u' => (10, 0),
                        'x' => (16, 0),
                        'X' => (16, F_UPPER),
                        _ => (8, 0), // 'o'
                    };
                    self.gen_expr(&args[i])?;
                    self.asm.mov_reg(0, RES);
                    self.asm.load_imm(1, radix);
                    self.asm.load_imm(2, flags | extra);
                    self.asm.load_imm(3, width);
                    self.asm.load_imm(4, prec);
                    let l = self.fs_routine("FmtInt");
                    self.asm.bl(l);
                }
                'c' => {
                    let charbuf = self.fs_scratch()[2];
                    self.gen_expr(&args[i])?;
                    self.asm.adr_global_fs(5, charbuf);
                    self.asm.store_mem(RES, 5, 1);
                    self.asm.adr_global_fs(0, charbuf);
                    self.asm.load_imm(1, 1); // length 1
                    self.asm.load_imm(2, flags);
                    self.asm.load_imm(3, width);
                    self.asm.load_imm(4, -1); // %c ignores precision
                    let l = self.fs_routine("FmtStr");
                    self.asm.bl(l);
                }
                's' => {
                    self.gen_expr(&args[i])?;
                    self.asm.mov_reg(0, RES);
                    self.asm.load_imm(1, -1); // length -1 ⇒ strlen
                    self.asm.load_imm(2, flags);
                    self.asm.load_imm(3, width);
                    self.asm.load_imm(4, prec);
                    let l = self.fs_routine("FmtStr");
                    self.asm.bl(l);
                }
                'f' => {
                    self.gen_fexpr(&args[i])?; // FRES = value
                    self.asm.fmov_reg(0, FRES); // d0 = value
                    self.asm.load_imm(0, if prec < 0 { 6 } else { prec }); // %f default precision 6
                    self.asm.load_imm(1, flags);
                    self.asm.load_imm(2, width);
                    let l = self.fs_routine("FmtFloat");
                    self.asm.bl(l);
                }
                'e' | 'E' | 'g' | 'G' => {
                    let conv = match spec.conv {
                        'e' => 1,
                        'E' => 1 | 4,
                        'g' => 2,
                        _ => 2 | 4, // 'G'
                    };
                    self.gen_fexpr(&args[i])?;
                    self.asm.fmov_reg(0, FRES); // d0 = value
                    self.asm.load_imm(0, if prec < 0 { 6 } else { prec });
                    self.asm.load_imm(1, flags);
                    self.asm.load_imm(2, width);
                    self.asm.load_imm(3, conv);
                    let l = self.fs_routine("FmtFloatEg");
                    self.asm.bl(l);
                }
                other => {
                    return Err(CodegenError::new(
                        format!(
                            "freestanding aarch64-linux: printf conversion %{other} is not \
                             supported yet (%e/%g pending)"
                        ),
                        None,
                    ));
                }
            }
        }
        self.emit_literal_fs(&lit);
        Ok(())
    }

    /// Write a literal run through the output sink (so it honours a `StrPrint`
    /// redirect): intern it in `__text`, point `x1`/`x2` at it, call `OutWrite`.
    fn emit_literal_fs(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        let idx = self.asm.intern_string(s);
        self.asm.adr(1, idx); // x1 = &string
        self.asm.load_imm(2, s.len() as i64); // x2 = byte length
        let l = self.fs_routine("OutWrite");
        self.asm.bl(l);
    }

    /// The four print scratch buffers (`digbuf`, `outbuf`, `charbuf`, `out_ptr`),
    /// allocated once in BSS.
    fn fs_scratch(&mut self) -> [u64; 4] {
        if let Some(s) = self.fs_scratch_off {
            return s;
        }
        let s = [
            self.alloc_bss_fs(FS_INT_DIGBUF, 8),
            self.alloc_bss_fs(FS_OUTBUF, 8),
            self.alloc_bss_fs(8, 8),
            self.alloc_bss_fs(8, 8),
        ];
        self.fs_scratch_off = Some(s);
        s
    }

    /// Test `flags & flag` (using `x7` as scratch) and branch to `label` when the
    /// bit is set (`if_set`) or clear.
    fn fs_tst(&mut self, flags_reg: u32, flag: i64, label: usize, if_set: bool) {
        self.asm.load_imm(7, flag);
        self.asm.and(7, flags_reg, 7);
        if if_set {
            self.asm.cbnz(7, label);
        } else {
            self.asm.cbz(7, label);
        }
    }

    /// The output sink (`x1`=buf, `x2`=len): to stdout via `write` when the
    /// `out_ptr` global is 0, else append to `[out_ptr]` and advance it (the
    /// `StrPrint` path). A leaf — clobbers only `x0`/`x8..x12`.
    fn emit_fs_out_write(&mut self) {
        let out_ptr = self.fs_scratch()[3];
        let stdout = self.asm.new_label();
        let loop_ = self.asm.new_label();
        let done = self.asm.new_label();
        self.asm.adr_global_fs(9, out_ptr);
        self.asm.load_mem(10, 9, 8, false); // x10 = out_ptr
        self.asm.cbz(10, stdout);
        self.asm.place(loop_);
        self.asm.cbz(2, done);
        self.asm.load_mem(12, 1, 1, false);
        self.asm.store_mem(12, 10, 1);
        self.asm.add_imm(1, 1, 1);
        self.asm.add_imm(10, 10, 1);
        self.asm.sub_imm(2, 2, 1);
        self.asm.b(loop_);
        self.asm.place(done);
        self.asm.store_mem(10, 9, 8); // out_ptr = new cursor
        self.asm.ret();
        self.asm.place(stdout);
        // Loop until the whole buffer is written: `write` may return a short
        // count, and `-EINTR` (-4) means retry. x1=buf, x2=remaining.
        let wloop = self.asm.new_label();
        let advance = self.asm.new_label();
        let wdone = self.asm.new_label();
        self.asm.place(wloop);
        self.asm.cbz(2, wdone); // nothing left
        self.asm.load_imm(0, 1); // fd = stdout
        self.asm.load_imm(8, 64); // SYS_write
        self.asm.svc(); // x0 = bytes written (or -errno)
        self.asm.cmp_imm0(0);
        self.asm.b_cond(COND_GT, advance); // wrote >0 bytes
        self.asm.add_imm(5, 0, 4); // x5 = x0 + 4 (==0 iff x0 == -EINTR)
        self.asm.cbz(5, wloop); // EINTR: retry same buf/len
        self.asm.b(wdone); // other error / 0: give up
        self.asm.place(advance);
        self.asm.add(1, 1, 0); // buf += written
        self.asm.sub(2, 2, 0); // remaining -= written
        self.asm.b(wloop);
        self.asm.place(wdone);
        self.asm.ret();
    }

    /// The integer formatter (mirrors `fmt::render_int`): `x0`=value, `x1`=radix,
    /// `x2`=flags, `x3`=width, `x4`=precision. Builds the field in `outbuf` and
    /// hands it to `OutWrite`. State lives in `x9..x17`; all of it is dead by the
    /// single sink call, so only `lr` is preserved.
    fn emit_fs_fmt_int(&mut self) {
        let [digbuf, outbuf, _c, _o] = self.fs_scratch();
        self.asm.push(LR);
        self.asm.mov_reg(9, 1); // radix
        self.asm.mov_reg(10, 2); // flags
        self.asm.mov_reg(11, 3); // width
        self.asm.mov_reg(12, 4); // precision
        // ---- sign char (x13) + magnitude (x0; loop value in x8) ----
        self.asm.load_imm(13, 0);
        let unsigned = self.asm.new_label();
        let havesign = self.asm.new_label();
        let nonneg = self.asm.new_label();
        let tryspace = self.asm.new_label();
        self.fs_tst(10, F_SIGNED, unsigned, false);
        self.asm.cmp_imm0(0);
        self.asm.b_cond(COND_GE, nonneg);
        self.asm.neg(0, 0);
        self.asm.load_imm(13, b'-' as i64);
        self.asm.b(havesign);
        self.asm.place(nonneg);
        self.fs_tst(10, F_PLUS, tryspace, false);
        self.asm.load_imm(13, b'+' as i64);
        self.asm.b(havesign);
        self.asm.place(tryspace);
        self.fs_tst(10, F_SPACE, havesign, false);
        self.asm.load_imm(13, b' ' as i64);
        self.asm.place(havesign);
        self.asm.place(unsigned);
        // ---- digits into digbuf, right-to-left (x14 = cursor) ----
        self.asm.adr_global_fs(14, digbuf + FS_INT_DIGBUF);
        self.asm.mov_reg(8, 0); // loop value (keep x0 = original magnitude)
        let dloop = self.asm.new_label();
        let store = self.asm.new_label();
        let upper = self.asm.new_label();
        self.asm.place(dloop);
        self.asm.udiv(5, 8, 9); // q
        self.asm.msub(6, 5, 9, 8); // rem
        self.asm.add_imm(6, 6, b'0' as u32);
        self.asm.cmp_imm(6, b'9' as u32);
        self.asm.b_cond(COND_LS, store);
        self.fs_tst(10, F_UPPER, upper, true);
        self.asm.add_imm(6, 6, 0x27); // 'a'..'f'
        self.asm.b(store);
        self.asm.place(upper);
        self.asm.add_imm(6, 6, 0x07); // 'A'..'F'
        self.asm.place(store);
        self.asm.sub_imm(14, 14, 1);
        self.asm.store_mem(6, 14, 1);
        self.asm.mov_reg(8, 5); // value = q
        self.asm.cbnz(8, dloop);
        // x15 = digit count = (digbuf+80) - x14
        self.asm.adr_global_fs(15, digbuf + FS_INT_DIGBUF);
        self.asm.sub(15, 15, 14);
        // ---- octal `#`: ensure a leading 0 ----
        let nooct = self.asm.new_label();
        self.asm.cmp_imm(9, 8);
        self.asm.b_cond(COND_NE, nooct);
        self.fs_tst(10, F_HASH, nooct, false);
        self.asm.load_mem(5, 14, 1, false);
        self.asm.cmp_imm(5, b'0' as u32);
        self.asm.b_cond(COND_EQ, nooct);
        self.asm.sub_imm(14, 14, 1);
        self.asm.load_imm(5, b'0' as i64);
        self.asm.store_mem(5, 14, 1);
        self.asm.add_imm(15, 15, 1);
        self.asm.place(nooct);
        // ---- precision (min digits); 0 value with precision 0 ⇒ no digits ----
        let precdone = self.asm.new_label();
        let precpad = self.asm.new_label();
        let ploop = self.asm.new_label();
        self.asm.cmp_imm0(12);
        self.asm.b_cond(COND_LT, precdone); // precision −1 ⇒ none
        self.asm.b_cond(COND_NE, precpad);
        // octal `#` keeps a leading 0 even at precision 0 of value 0 (don't drop).
        let not_octhash = self.asm.new_label();
        self.asm.cmp_imm(9, 8);
        self.asm.b_cond(COND_NE, not_octhash);
        self.fs_tst(10, F_HASH, precdone, true);
        self.asm.place(not_octhash);
        self.asm.cmp_imm(15, 1);
        self.asm.b_cond(COND_NE, precpad);
        self.asm.load_mem(5, 14, 1, false);
        self.asm.cmp_imm(5, b'0' as u32);
        self.asm.b_cond(COND_NE, precpad);
        self.asm.add_imm(14, 14, 1); // drop the single '0'
        self.asm.load_imm(15, 0);
        self.asm.b(precdone);
        self.asm.place(precpad);
        self.asm.place(ploop);
        self.asm.cmp_reg(15, 12);
        self.asm.b_cond(COND_GE, precdone);
        self.asm.sub_imm(14, 14, 1);
        self.asm.load_imm(5, b'0' as i64);
        self.asm.store_mem(5, 14, 1);
        self.asm.add_imm(15, 15, 1);
        self.asm.b(ploop);
        self.asm.place(precdone);
        // ---- alt length (x16): `0x`/`0X` for `#` hex of a non-zero value ----
        let noalt = self.asm.new_label();
        self.asm.load_imm(16, 0);
        self.fs_tst(10, F_HASH, noalt, false);
        self.asm.cmp_imm(9, 16);
        self.asm.b_cond(COND_NE, noalt);
        self.asm.cbz(0, noalt); // original magnitude == 0
        self.asm.load_imm(16, 2);
        self.asm.place(noalt);
        // ---- assemble into outbuf (x17 = cursor) ----
        self.asm.adr_global_fs(17, outbuf);
        // body_len (x5) = digits + alt + (sign ? 1 : 0)
        self.asm.add(5, 15, 16);
        let nosl = self.asm.new_label();
        self.asm.cbz(13, nosl);
        self.asm.add_imm(5, 5, 1);
        self.asm.place(nosl);
        // pad (x6) = max(0, width − body_len)
        let padok = self.asm.new_label();
        self.asm.sub(6, 11, 5);
        self.asm.cmp_imm0(6);
        self.asm.b_cond(COND_GE, padok);
        self.asm.load_imm(6, 0);
        self.asm.place(padok);
        // choose justification
        let do_minus = self.asm.new_label();
        let do_right = self.asm.new_label();
        let donebody = self.asm.new_label();
        self.fs_tst(10, F_MINUS, do_minus, true);
        self.fs_tst(10, F_ZERO, do_right, false);
        self.asm.cmp_imm0(12); // zero flag ignored when precision is given
        self.asm.b_cond(COND_GE, do_right);
        // zero-justify: sign, alt, zeros, digits
        self.fs_append_sign();
        self.fs_append_alt();
        self.fs_append_pad(b'0');
        self.fs_append_digits();
        self.asm.b(donebody);
        // right-justify: spaces, sign, alt, digits
        self.asm.place(do_right);
        self.fs_append_pad(b' ');
        self.fs_append_sign();
        self.fs_append_alt();
        self.fs_append_digits();
        self.asm.b(donebody);
        // left-justify: sign, alt, digits, spaces
        self.asm.place(do_minus);
        self.fs_append_sign();
        self.fs_append_alt();
        self.fs_append_digits();
        self.fs_append_pad(b' ');
        self.asm.place(donebody);
        // OutWrite(outbuf, cursor − outbuf)
        self.asm.adr_global_fs(1, outbuf);
        self.asm.sub(2, 17, 1);
        let ow = self.fs_routine("OutWrite");
        self.asm.bl(ow);
        self.asm.pop(LR);
        self.asm.ret();
    }

    // Append helpers for `emit_fs_fmt_int`: the outbuf cursor is x17, the sign char
    // x13, the alt length x16, the flags x10, the digit run [x14, x14+x15), and the
    // pad count x6. (All fixed by the routine above.)
    fn fs_append_sign(&mut self) {
        let skip = self.asm.new_label();
        self.asm.cbz(13, skip);
        self.asm.store_mem(13, 17, 1);
        self.asm.add_imm(17, 17, 1);
        self.asm.place(skip);
    }
    fn fs_append_alt(&mut self) {
        let skip = self.asm.new_label();
        let wrote = self.asm.new_label();
        self.asm.cbz(16, skip);
        self.asm.load_imm(5, b'0' as i64);
        self.asm.store_mem(5, 17, 1);
        self.asm.add_imm(17, 17, 1);
        self.asm.load_imm(5, b'x' as i64);
        self.fs_tst(10, F_UPPER, wrote, false);
        self.asm.load_imm(5, b'X' as i64);
        self.asm.place(wrote);
        self.asm.store_mem(5, 17, 1);
        self.asm.add_imm(17, 17, 1);
        self.asm.place(skip);
    }
    fn fs_append_pad(&mut self, fill: u8) {
        let top = self.asm.new_label();
        let done = self.asm.new_label();
        self.asm.load_imm(5, fill as i64);
        self.asm.place(top);
        self.asm.cbz(6, done);
        self.asm.store_mem(5, 17, 1);
        self.asm.add_imm(17, 17, 1);
        self.asm.sub_imm(6, 6, 1);
        self.asm.b(top);
        self.asm.place(done);
    }
    fn fs_append_digits(&mut self) {
        let top = self.asm.new_label();
        let done = self.asm.new_label();
        self.asm.mov_reg(5, 15); // count (x6 holds the pad count for a later append)
        self.asm.place(top);
        self.asm.cbz(5, done);
        self.asm.load_mem(7, 14, 1, false);
        self.asm.store_mem(7, 17, 1);
        self.asm.add_imm(14, 14, 1);
        self.asm.add_imm(17, 17, 1);
        self.asm.sub_imm(5, 5, 1);
        self.asm.b(top);
        self.asm.place(done);
    }

    /// The string/char formatter (mirrors `fmt::render_str`): `x0`=ptr,
    /// `x1`=len (−1 ⇒ strlen), `x2`=flags, `x3`=width, `x4`=precision. Applies a
    /// precision (truncate) then pads to width (left-justified with `-`). The
    /// surviving state across the two sink calls (ptr/len/pad) lives in callee-saved
    /// `x19..x21`.
    fn emit_fs_fmt_str(&mut self) {
        self.asm.push(19);
        self.asm.push(20);
        self.asm.push(21);
        self.asm.push(LR);
        self.asm.mov_reg(9, 0); // ptr
        self.asm.mov_reg(10, 1); // len
        self.asm.mov_reg(11, 2); // flags
        self.asm.mov_reg(12, 3); // width
        self.asm.mov_reg(13, 4); // precision
        // len: strlen when negative
        let havelen = self.asm.new_label();
        let slloop = self.asm.new_label();
        let sldone = self.asm.new_label();
        self.asm.cmp_imm0(10);
        self.asm.b_cond(COND_GE, havelen);
        self.asm.mov_reg(5, 9); // cursor
        self.asm.place(slloop);
        self.asm.load_mem(6, 5, 1, false);
        self.asm.cbz(6, sldone);
        self.asm.add_imm(5, 5, 1);
        self.asm.b(slloop);
        self.asm.place(sldone);
        self.asm.sub(10, 5, 9);
        self.asm.place(havelen);
        // precision: clamp len when 0 ≤ prec < len
        let noprec = self.asm.new_label();
        self.asm.cmp_imm0(13);
        self.asm.b_cond(COND_LT, noprec);
        self.asm.cmp_reg(13, 10);
        self.asm.b_cond(COND_GE, noprec);
        self.asm.mov_reg(10, 13);
        self.asm.place(noprec);
        // pad (x14) = max(0, width − len)
        let padok = self.asm.new_label();
        self.asm.sub(14, 12, 10);
        self.asm.cmp_imm0(14);
        self.asm.b_cond(COND_GE, padok);
        self.asm.load_imm(14, 0);
        self.asm.place(padok);
        // survivors -> callee-saved (out_write clobbers x9..x12)
        self.asm.mov_reg(19, 9); // ptr
        self.asm.mov_reg(20, 10); // len
        self.asm.mov_reg(21, 14); // pad
        let do_minus = self.asm.new_label();
        let done = self.asm.new_label();
        self.fs_tst(11, F_MINUS, do_minus, true);
        self.fs_str_pad();
        self.fs_str_body();
        self.asm.b(done);
        self.asm.place(do_minus);
        self.fs_str_body();
        self.fs_str_pad();
        self.asm.place(done);
        self.asm.pop(LR);
        self.asm.pop(21);
        self.asm.pop(20);
        self.asm.pop(19);
        self.asm.ret();
    }
    /// Output the body (`x19`=ptr, `x20`=len) through the sink.
    fn fs_str_body(&mut self) {
        self.asm.mov_reg(1, 19);
        self.asm.mov_reg(2, 20);
        let ow = self.fs_routine("OutWrite");
        self.asm.bl(ow);
    }
    /// Output `x21` space padding through the sink, in `outbuf`-sized chunks.
    fn fs_str_pad(&mut self) {
        let outbuf = self.fs_scratch()[1];
        let outer = self.asm.new_label();
        let done = self.asm.new_label();
        let noclamp = self.asm.new_label();
        let fill = self.asm.new_label();
        let filled = self.asm.new_label();
        self.asm.place(outer);
        self.asm.cbz(21, done);
        // chunk x5 = min(x21, 1024)
        self.asm.mov_reg(5, 21);
        self.asm.cmp_imm(5, 1024);
        self.asm.b_cond(COND_LS, noclamp);
        self.asm.load_imm(5, 1024);
        self.asm.place(noclamp);
        self.asm.sub(21, 21, 5);
        // fill outbuf[0..chunk] with spaces (cursor x6, counter x7, char x9)
        self.asm.adr_global_fs(6, outbuf);
        self.asm.mov_reg(7, 5);
        self.asm.load_imm(9, b' ' as i64);
        self.asm.place(fill);
        self.asm.cbz(7, filled);
        self.asm.store_mem(9, 6, 1);
        self.asm.add_imm(6, 6, 1);
        self.asm.sub_imm(7, 7, 1);
        self.asm.b(fill);
        self.asm.place(filled);
        self.asm.adr_global_fs(1, outbuf);
        self.asm.mov_reg(2, 5);
        let ow = self.fs_routine("OutWrite");
        self.asm.bl(ow);
        self.asm.b(outer);
        self.asm.place(done);
    }

    /// Freestanding lowering of a libc-backed builtin: inline scalar ops, or a call
    /// to an emitted runtime routine (same ABI as the libc function it replaces).
    fn gen_builtin_fs(&mut self, name: &str, args: &[Expr], pos: Pos) -> Result<(), CodegenError> {
        match name {
            // `Abs`: negate when negative.
            "Abs" => {
                self.gen_expr(&args[0])?;
                let skip = self.asm.new_label();
                self.asm.cmp_imm0(RES);
                self.asm.b_cond(COND_GE, skip);
                self.asm.neg(RES, RES);
                self.asm.place(skip);
                return Ok(());
            }
            // ASCII case conversion: shift a letter by 32.
            "ToUpper" | "ToLower" => {
                self.gen_expr(&args[0])?;
                let upper = name == "ToUpper";
                let lo = if upper { b'a' } else { b'A' } as u32;
                let skip = self.asm.new_label();
                self.asm.sub_imm(T2, RES, lo); // T2 = c - 'a'/'A'
                self.asm.cmp_imm(T2, 25);
                self.asm.b_cond(COND_HI, skip); // not a letter in range
                if upper {
                    self.asm.sub_imm(RES, RES, 32);
                } else {
                    self.asm.add_imm(RES, RES, 32);
                }
                self.asm.place(skip);
                return Ok(());
            }
            // Algebraic F64 math — single AArch64 FP instructions.
            "Sqrt" | "Fabs" | "Floor" | "Ceil" | "Round" => {
                self.gen_fexpr(&args[0])?; // FRES = value
                match name {
                    "Sqrt" => self.asm.fsqrt(FRES, FRES),
                    "Fabs" => self.asm.fabs(FRES, FRES),
                    "Floor" => self.asm.frintm(FRES, FRES),
                    "Ceil" => self.asm.frintp(FRES, FRES),
                    _ => self.asm.frinta(FRES, FRES), // Round (ties away)
                }
                return Ok(());
            }
            _ => {}
        }
        // Routine-backed builtins (standard ABI: args in x0.., result in x0).
        let sname: &'static str = match name {
            "MAlloc" => "MAlloc",
            "Free" => "Free",
            "StrLen" => "StrLen",
            "StrCpy" => "StrCpy",
            "StrCmp" => "StrCmp",
            "StrCat" => "StrCat",
            "StrFind" => "StrFind",
            "MemCpy" => "MemCpy",
            "MemSet" => "MemSet",
            "MemCmp" => "MemCmp",
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
        let ret = crate::builtins::ret_of(name).unwrap_or(Type::I64);
        let label = self.fs_routine(sname);
        self.emit_call(CallTarget::Label(label), &params, args, &ret, name, pos)
    }

    /// Emit the freestanding runtime routines the program uses, once, in a fixed
    /// order. Each follows the internal ABI (args `x0..`, result `x0`) and clobbers
    /// only caller-saved registers, so it is a safe `bl` target.
    fn emit_fs_runtime(&mut self) {
        const ORDER: &[&str] = &[
            "OutWrite",
            "FmtInt",
            "FmtStr",
            "FmtFloat",
            "FmtFloatEg",
            "BnMul",
            "BnDiv10",
            "BnShl",
            "BnShr",
            "MAlloc",
            "Free",
            "StrLen",
            "StrCpy",
            "StrCmp",
            "StrCat",
            "StrFind",
            "MemCpy",
            "MemSet",
            "MemCmp",
        ];
        for &name in ORDER {
            let Some(&l) = self.fs_routines.get(name) else {
                continue;
            };
            self.asm.place(l);
            match name {
                "OutWrite" => self.emit_fs_out_write(),
                "FmtInt" => self.emit_fs_fmt_int(),
                "FmtStr" => self.emit_fs_fmt_str(),
                "FmtFloat" => self.emit_fs_fmt_float(),
                "FmtFloatEg" => self.emit_fs_fmt_float_eg(),
                "BnMul" => self.emit_fs_bn_mul(),
                "BnDiv10" => self.emit_fs_bn_div10(),
                "BnShl" => self.emit_fs_bn_shl(),
                "BnShr" => self.emit_fs_bn_shr(),
                "MAlloc" => self.emit_fs_malloc(),
                "Free" => self.asm.ret(), // a no-op bump allocator never frees
                "StrLen" => self.emit_fs_strlen(),
                "StrCpy" => self.emit_fs_strcpy(),
                "StrCmp" => self.emit_fs_strcmp(),
                "StrCat" => self.emit_fs_strcat(),
                "StrFind" => self.emit_fs_strfind(),
                "MemCpy" => self.emit_fs_memcpy(),
                "MemSet" => self.emit_fs_memset(),
                "MemCmp" => self.emit_fs_memcmp(),
                _ => {}
            }
        }
    }

    /// `MAlloc(x0=n) -> x0`: a bump allocator over `mmap`'d chunks (≥1 MiB,
    /// page-aligned), 16-byte-aligned allocations, state in two BSS words. `Free`
    /// is a no-op, so chunks are never reused.
    fn emit_fs_malloc(&mut self) {
        let hp = self.alloc_bss_fs(8, 8); // heap bump pointer
        let he = self.alloc_bss_fs(8, 8); // heap end
        let fits = self.asm.new_label();
        let sized = self.asm.new_label();
        // x9 = (n + 15) & ~15
        self.asm.add_imm(9, 0, 15);
        self.asm.load_imm(10, -16);
        self.asm.and(9, 9, 10);
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
        self.asm.mov_reg(0, 11);
        self.asm.ret();
    }

    /// `StrLen(x0=ptr) -> x0` — count bytes to the NUL.
    fn emit_fs_strlen(&mut self) {
        let l = self.asm.new_label();
        let d = self.asm.new_label();
        self.asm.mov_reg(9, 0); // cursor
        self.asm.place(l);
        self.asm.load_mem(10, 9, 1, false);
        self.asm.cbz(10, d);
        self.asm.add_imm(9, 9, 1);
        self.asm.b(l);
        self.asm.place(d);
        self.asm.sub(0, 9, 0); // cursor - ptr
        self.asm.ret();
    }

    /// `StrCpy(x0=dst, x1=src) -> x0` — copy through the NUL; return dst.
    fn emit_fs_strcpy(&mut self) {
        let l = self.asm.new_label();
        let d = self.asm.new_label();
        self.asm.mov_reg(9, 0); // dst cursor (x0 kept for the return)
        self.asm.place(l);
        self.asm.load_mem(10, 1, 1, false);
        self.asm.store_mem(10, 9, 1);
        self.asm.cbz(10, d);
        self.asm.add_imm(9, 9, 1);
        self.asm.add_imm(1, 1, 1);
        self.asm.b(l);
        self.asm.place(d);
        self.asm.ret();
    }

    /// `StrCmp(x0=a, x1=b) -> x0` — a sign in {-1, 0, 1} (matches the interpreter).
    fn emit_fs_strcmp(&mut self) {
        let l = self.asm.new_label();
        let diff = self.asm.new_label();
        let eq = self.asm.new_label();
        let done = self.asm.new_label();
        self.asm.place(l);
        self.asm.load_mem(9, 0, 1, false);
        self.asm.load_mem(10, 1, 1, false);
        self.asm.cmp_reg(9, 10);
        self.asm.b_cond(COND_NE, diff);
        self.asm.cbz(9, eq); // both NUL -> equal
        self.asm.add_imm(0, 0, 1);
        self.asm.add_imm(1, 1, 1);
        self.asm.b(l);
        self.asm.place(diff);
        self.asm.load_imm(0, 1);
        self.asm.cmp_reg(9, 10);
        self.asm.b_cond(COND_HI, done); // a > b (unsigned byte)
        self.asm.load_imm(0, -1);
        self.asm.place(done);
        self.asm.ret();
        self.asm.place(eq);
        self.asm.load_imm(0, 0);
        self.asm.ret();
    }

    /// `StrCat(x0=dst, x1=src) -> x0` — append src at dst's NUL; return dst.
    fn emit_fs_strcat(&mut self) {
        let fe = self.asm.new_label();
        let cp = self.asm.new_label();
        let d = self.asm.new_label();
        self.asm.mov_reg(9, 0); // dst cursor
        self.asm.place(fe);
        self.asm.load_mem(10, 9, 1, false);
        self.asm.cbz(10, cp);
        self.asm.add_imm(9, 9, 1);
        self.asm.b(fe);
        self.asm.place(cp);
        self.asm.load_mem(10, 1, 1, false);
        self.asm.store_mem(10, 9, 1);
        self.asm.cbz(10, d);
        self.asm.add_imm(9, 9, 1);
        self.asm.add_imm(1, 1, 1);
        self.asm.b(cp);
        self.asm.place(d);
        self.asm.ret();
    }

    /// `MemCpy(x0=dst, x1=src, x2=n) -> x0` — copy n bytes; return dst.
    fn emit_fs_memcpy(&mut self) {
        let l = self.asm.new_label();
        let d = self.asm.new_label();
        self.asm.mov_reg(9, 0); // dst cursor
        self.asm.place(l);
        self.asm.cbz(2, d);
        self.asm.load_mem(10, 1, 1, false);
        self.asm.store_mem(10, 9, 1);
        self.asm.add_imm(9, 9, 1);
        self.asm.add_imm(1, 1, 1);
        self.asm.sub_imm(2, 2, 1);
        self.asm.b(l);
        self.asm.place(d);
        self.asm.ret();
    }

    /// `MemSet(x0=dst, x1=c, x2=n) -> x0` — set n bytes to c; return dst.
    fn emit_fs_memset(&mut self) {
        let l = self.asm.new_label();
        let d = self.asm.new_label();
        self.asm.mov_reg(9, 0); // dst cursor
        self.asm.place(l);
        self.asm.cbz(2, d);
        self.asm.store_mem(1, 9, 1); // low byte of c
        self.asm.add_imm(9, 9, 1);
        self.asm.sub_imm(2, 2, 1);
        self.asm.b(l);
        self.asm.place(d);
        self.asm.ret();
    }

    /// BSS for the float formatter: the 48-limb `BIGNUM` and the 768-byte digit
    /// buffer, allocated once.
    fn fs_float_scratch(&mut self) -> (u64, u64) {
        if let Some(s) = self.fs_float_off {
            return s;
        }
        let s = (
            self.alloc_bss_fs(NLIMBS as u64 * 8, 8),
            self.alloc_bss_fs(FS_FLOAT_DIGBUF, 1),
        );
        self.fs_float_off = Some(s);
        s
    }

    /// `BIGNUM *= x0` (a small multiplier). Carry propagates low→high.
    fn emit_fs_bn_mul(&mut self) {
        let (bn, _) = self.fs_float_scratch();
        let loop_l = self.asm.new_label();
        self.asm.adr_global_fs(8, bn);
        self.asm.load_imm(9, 0); // carry
        self.asm.load_imm(10, 0); // index
        self.asm.place(loop_l);
        self.asm.ldr_idx8(11, 8, 10);
        self.asm.mul(12, 11, 0); // lo = limb * k
        self.asm.umulh(13, 11, 0); // hi
        self.asm.adds(12, 12, 9); // lo += carry
        self.asm.adc(13, 13, 31); // hi += C (x31 = XZR)
        self.asm.str_idx8(12, 8, 10);
        self.asm.mov_reg(9, 13); // new carry
        self.asm.add_imm(10, 10, 1);
        self.asm.cmp_imm(10, NLIMBS as u32);
        self.asm.b_cond(COND_LO, loop_l);
        self.asm.ret();
    }

    /// `BIGNUM /= 10`, returning the remainder digit in `x0`. Most-significant limb
    /// first; each limb is divided by a 64-iteration shift/subtract (AArch64 has no
    /// 128÷64), with the running remainder (< 10) carried down.
    fn emit_fs_bn_div10(&mut self) {
        let (bn, _) = self.fs_float_scratch();
        let outer = self.asm.new_label();
        let bits = self.asm.new_label();
        let nosub = self.asm.new_label();
        let done = self.asm.new_label();
        self.asm.adr_global_fs(8, bn);
        self.asm.load_imm(9, NLIMBS as i64 - 1); // index (MSB first)
        self.asm.load_imm(10, 0); // running remainder
        self.asm.place(outer);
        self.asm.ldr_idx8(11, 8, 9); // limb
        self.asm.load_imm(12, 0); // quotient
        self.asm.load_imm(13, 64); // bit counter
        self.asm.place(bits);
        self.asm.lsl_imm(10, 10, 1); // rem <<= 1
        self.asm.lsr_imm(14, 11, 63); // top bit of limb
        self.asm.orr(10, 10, 14);
        self.asm.lsl_imm(11, 11, 1); // consume top bit
        self.asm.lsl_imm(12, 12, 1); // quot <<= 1
        self.asm.cmp_imm(10, 10);
        self.asm.b_cond(COND_LO, nosub); // rem < 10
        self.asm.sub_imm(10, 10, 10); // rem -= 10
        self.asm.add_imm(12, 12, 1); // quot |= 1
        self.asm.place(nosub);
        self.asm.sub_imm(13, 13, 1);
        self.asm.cbnz(13, bits);
        self.asm.str_idx8(12, 8, 9); // store quotient
        self.asm.cbz(9, done); // processed limb 0 → finished
        self.asm.sub_imm(9, 9, 1);
        self.asm.b(outer);
        self.asm.place(done);
        self.asm.mov_reg(0, 10); // remainder
        self.asm.ret();
    }

    /// `BIGNUM <<= x0` bits.
    fn emit_fs_bn_shl(&mut self) {
        let (bn, _) = self.fs_float_scratch();
        let loop_l = self.asm.new_label();
        let zero = self.asm.new_label();
        let store = self.asm.new_label();
        self.asm.lsr_imm(9, 0, 6); // word = bits/64
        self.asm.and_imm_lowbits(10, 0, 6); // bit = bits%64
        self.asm.adr_global_fs(8, bn);
        self.asm.load_imm(11, NLIMBS as i64 - 1); // i (high → low)
        self.asm.place(loop_l);
        self.asm.subs(12, 11, 9); // src = i - word
        self.asm.b_cond(COND_LT, zero);
        self.asm.ldr_idx8(13, 8, 12); // lo
        self.asm.cbz(10, store); // bit == 0 → just lo
        self.asm.lslv(13, 13, 10); // lo << bit
        self.asm.sub_imm(12, 12, 1); // src - 1
        self.asm.cmp_imm0(12);
        self.asm.b_cond(COND_LT, store);
        self.asm.ldr_idx8(14, 8, 12); // hi
        self.asm.load_imm(15, 64);
        self.asm.sub(15, 15, 10); // 64 - bit
        self.asm.lsrv(14, 14, 15); // hi >> (64-bit)
        self.asm.orr(13, 13, 14);
        self.asm.b(store);
        self.asm.place(zero);
        self.asm.load_imm(13, 0);
        self.asm.place(store);
        self.asm.str_idx8(13, 8, 11);
        self.asm.sub_imm(11, 11, 1);
        self.asm.cmp_imm0(11);
        self.asm.b_cond(COND_GE, loop_l);
        self.asm.ret();
    }

    /// `BIGNUM >>= x0` bits, rounding the dropped bits to nearest, ties to even.
    fn emit_fs_bn_shr(&mut self) {
        let (bn, _) = self.fs_float_scratch();
        self.asm.adr_global_fs(8, bn); // base
        self.asm.mov_reg(9, 0); // bits
        // ---- round bit (x15) + sticky (x16) of the dropped low `bits` bits ----
        self.asm.sub_imm(12, 9, 1); // m = bits - 1
        self.asm.lsr_imm(13, 12, 6); // mword
        self.asm.and_imm_lowbits(14, 12, 6); // mbit
        self.asm.ldr_idx8(15, 8, 13); // limb[mword]
        self.asm.mov_reg(16, 15); // copy for sticky
        self.asm.lsrv(15, 15, 14);
        self.asm.and_imm_lowbits(15, 15, 1); // round bit
        self.asm.load_imm(17, 1);
        self.asm.lslv(17, 17, 14);
        self.asm.sub_imm(17, 17, 1); // mask = (1<<mbit)-1
        self.asm.and(16, 16, 17); // partial low bits
        let sloop = self.asm.new_label();
        let sdone = self.asm.new_label();
        self.asm.load_imm(18, 0); // j
        self.asm.place(sloop);
        self.asm.cmp_reg(18, 13);
        self.asm.b_cond(COND_HS, sdone);
        self.asm.ldr_idx8(0, 8, 18);
        self.asm.orr(16, 16, 0);
        self.asm.add_imm(18, 18, 1);
        self.asm.b(sloop);
        self.asm.place(sdone);
        // ---- shift right by `bits` (word, bit) ----
        let shloop = self.asm.new_label();
        let lozero = self.asm.new_label();
        let havelo = self.asm.new_label();
        let store2 = self.asm.new_label();
        self.asm.lsr_imm(10, 9, 6); // word
        self.asm.and_imm_lowbits(11, 9, 6); // bit
        self.asm.load_imm(12, 0); // i (low → high)
        self.asm.place(shloop);
        self.asm.add(13, 12, 10); // src = i + word
        self.asm.cmp_imm(13, NLIMBS as u32);
        self.asm.b_cond(COND_HS, lozero);
        self.asm.ldr_idx8(14, 8, 13);
        self.asm.b(havelo);
        self.asm.place(lozero);
        self.asm.load_imm(14, 0);
        self.asm.place(havelo);
        self.asm.cbz(11, store2); // bit == 0
        self.asm.lsrv(14, 14, 11); // lo >> bit
        self.asm.add_imm(13, 13, 1); // src + 1
        self.asm.cmp_imm(13, NLIMBS as u32);
        self.asm.b_cond(COND_HS, store2);
        self.asm.ldr_idx8(0, 8, 13); // hi
        self.asm.load_imm(1, 64);
        self.asm.sub(1, 1, 11);
        self.asm.lslv(0, 0, 1); // hi << (64-bit)
        self.asm.orr(14, 14, 0);
        self.asm.place(store2);
        self.asm.str_idx8(14, 8, 12);
        self.asm.add_imm(12, 12, 1);
        self.asm.cmp_imm(12, NLIMBS as u32);
        self.asm.b_cond(COND_LO, shloop);
        // ---- round up if round_bit && (sticky || quotient is odd) ----
        let rdone = self.asm.new_label();
        let roundup = self.asm.new_label();
        let iloop = self.asm.new_label();
        self.asm.cbz(15, rdone); // round bit clear
        self.asm.cbnz(16, roundup); // sticky set
        self.asm.load_imm(12, 0);
        self.asm.ldr_idx8(0, 8, 12); // BIGNUM[0]
        self.asm.and_imm_lowbits(0, 0, 1);
        self.asm.cbz(0, rdone); // quotient even
        self.asm.place(roundup);
        self.asm.load_imm(12, 0); // i
        self.asm.place(iloop);
        self.asm.ldr_idx8(0, 8, 12);
        self.asm.add_imm(0, 0, 1);
        self.asm.str_idx8(0, 8, 12);
        self.asm.cbnz(0, rdone); // no wrap to 0 → no carry out
        self.asm.add_imm(12, 12, 1);
        self.asm.cmp_imm(12, NLIMBS as u32);
        self.asm.b_cond(COND_LO, iloop);
        self.asm.place(rdone);
        self.asm.ret();
    }

    /// `StrFind(x0=haystack, x1=needle) -> x0` — pointer to the first occurrence of
    /// `needle` in `haystack`, or NULL (libc `strstr`). An empty needle matches at
    /// the start.
    fn emit_fs_strfind(&mut self) {
        let outer = self.asm.new_label();
        let inner = self.asm.new_label();
        let next = self.asm.new_label();
        let found = self.asm.new_label();
        let notfound = self.asm.new_label();
        let ret_h = self.asm.new_label();
        // Empty needle -> return haystack.
        self.asm.load_mem(9, 1, 1, false);
        self.asm.cbz(9, ret_h);
        self.asm.place(outer);
        self.asm.load_mem(9, 0, 1, false);
        self.asm.cbz(9, notfound); // end of haystack
        self.asm.mov_reg(10, 0); // h cursor
        self.asm.mov_reg(11, 1); // n cursor
        self.asm.place(inner);
        self.asm.load_mem(12, 11, 1, false); // *n
        self.asm.cbz(12, found); // end of needle -> match
        self.asm.load_mem(13, 10, 1, false); // *h
        self.asm.cmp_reg(12, 13);
        self.asm.b_cond(COND_NE, next);
        self.asm.add_imm(10, 10, 1);
        self.asm.add_imm(11, 11, 1);
        self.asm.b(inner);
        self.asm.place(next);
        self.asm.add_imm(0, 0, 1);
        self.asm.b(outer);
        self.asm.place(found);
        self.asm.ret(); // x0 = match start
        self.asm.place(notfound);
        self.asm.load_imm(0, 0);
        self.asm.place(ret_h);
        self.asm.ret();
    }

    /// `%f` formatter (mirrors Rust's `{:.P}` byte-for-byte): `d0`=value, `x0`=P,
    /// `x1`=flags, `x2`=width. Builds `J = round(m·2^e·10^P)` in the BIGNUM, extracts
    /// its decimal digits, and places the point. Persistent state across the bignum
    /// calls lives in callee-saved `x19..x28`.
    fn emit_fs_fmt_float(&mut self) {
        let (bn, digbuf) = self.fs_float_scratch();
        let digend = digbuf + FS_FLOAT_DIGBUF;
        let outbuf = self.fs_scratch()[1];
        for r in [19, 20, 21, 22, 23, 24, 25, 26, 27, 28, LR] {
            self.asm.push(r);
        }
        self.asm.mov_reg(19, 0); // P
        self.asm.mov_reg(20, 1); // flags
        self.asm.mov_reg(21, 2); // width
        self.asm.fmov_to_gpr(23, 0); // value bits
        // ---- sign char (x22) + magnitude bits (x23) ----
        let signpos = self.asm.new_label();
        let havesign = self.asm.new_label();
        let nospace = self.asm.new_label();
        self.asm.load_imm(8, 0x7FFF_FFFF_FFFF_FFFF);
        self.asm.and(9, 23, 8); // magnitude bits
        self.asm.load_imm(22, 0); // sign = none
        self.asm.cbz(9, signpos); // ±0 is non-negative
        self.asm.lsr_imm(10, 23, 63);
        self.asm.cbz(10, signpos);
        self.asm.load_imm(22, b'-' as i64);
        self.asm.b(havesign);
        self.asm.place(signpos);
        self.fs_tst(20, F_PLUS, havesign, false);
        self.asm.load_imm(22, b'+' as i64);
        self.asm.place(havesign);
        self.asm.cbnz(22, nospace);
        self.fs_tst(20, F_SPACE, nospace, false);
        self.asm.load_imm(22, b' ' as i64);
        self.asm.place(nospace);
        self.asm.mov_reg(23, 9); // x23 = magnitude bits
        // ---- decompose into exp field (x10) + fraction (x9 → mantissa) ----
        let infnan = self.asm.new_label();
        let subnormal = self.asm.new_label();
        let havem = self.asm.new_label();
        self.asm.lsr_imm(10, 23, 52);
        self.asm.load_imm(8, 0x7FF);
        self.asm.and(10, 10, 8); // exp field
        self.asm.load_imm(8, 0x000F_FFFF_FFFF_FFFF);
        self.asm.and(9, 23, 8); // fraction
        self.asm.cmp_imm(10, 0x7FF);
        self.asm.b_cond(COND_EQ, infnan);
        self.asm.cbz(10, subnormal);
        // normal: m = frac | 2^52, e2 = exp - 1075
        self.asm.load_imm(8, 0x0010_0000_0000_0000);
        self.asm.orr(9, 9, 8);
        self.asm.sub_imm(10, 10, 1075); // careful: 1075 < 4096, sub_imm ok
        self.asm.b(havem);
        self.asm.place(subnormal);
        self.asm.load_imm(10, -1074); // e2 (m = frac)
        self.asm.place(havem);
        // s = e2 + P (kept in callee-saved x28 across the bignum calls)
        self.asm.add(28, 10, 19);
        // BIGNUM = m
        let zloop = self.asm.new_label();
        self.asm.adr_global_fs(8, bn);
        self.asm.load_imm(11, 0); // index
        self.asm.load_imm(12, 0); // zero
        self.asm.place(zloop);
        self.asm.str_idx8(12, 8, 11);
        self.asm.add_imm(11, 11, 1);
        self.asm.cmp_imm(11, NLIMBS as u32);
        self.asm.b_cond(COND_LO, zloop);
        self.asm.store_mem(9, 8, 8); // BIGNUM[0] = m
        // × 5^P
        let mulloop = self.asm.new_label();
        let muldone = self.asm.new_label();
        self.asm.mov_reg(24, 19); // counter = P (x24 free until ndig)
        self.asm.place(mulloop);
        self.asm.cbz(24, muldone);
        self.asm.load_imm(0, 5);
        let bm = self.fs_routine("BnMul");
        self.asm.bl(bm);
        self.asm.sub_imm(24, 24, 1);
        self.asm.b(mulloop);
        self.asm.place(muldone);
        // × 2^s
        let shifted = self.asm.new_label();
        let shrpath = self.asm.new_label();
        self.asm.cmp_imm0(28);
        self.asm.b_cond(COND_LT, shrpath);
        self.asm.mov_reg(0, 28);
        let sl = self.fs_routine("BnShl");
        self.asm.bl(sl);
        self.asm.b(shifted);
        self.asm.place(shrpath);
        self.asm.neg(0, 28);
        let sr = self.fs_routine("BnShr");
        self.asm.bl(sr);
        self.asm.place(shifted);
        // extract decimal digits into digbuf (downward from digend)
        let dloop = self.asm.new_label();
        let zchk = self.asm.new_label();
        let notzero = self.asm.new_label();
        let extracted = self.asm.new_label();
        self.asm.adr_global_fs(23, digend); // digit cursor
        self.asm.place(dloop);
        let bd = self.fs_routine("BnDiv10");
        self.asm.bl(bd); // x0 = digit
        self.asm.add_imm(0, 0, b'0' as u32);
        self.asm.sub_imm(23, 23, 1);
        self.asm.store_mem(0, 23, 1);
        self.asm.adr_global_fs(8, bn);
        self.asm.load_imm(9, 0);
        self.asm.place(zchk);
        self.asm.ldr_idx8(10, 8, 9);
        self.asm.cbnz(10, notzero);
        self.asm.add_imm(9, 9, 1);
        self.asm.cmp_imm(9, NLIMBS as u32);
        self.asm.b_cond(COND_LO, zchk);
        self.asm.b(extracted);
        self.asm.place(notzero);
        self.asm.b(dloop);
        self.asm.place(extracted);
        // ndig (x24) = digend - x23
        self.asm.adr_global_fs(8, digend);
        self.asm.sub(24, 8, 23);
        // bodylen (x25): P==0 ⇒ ndig; ndig>P ⇒ ndig+1; else P+2
        let pnz = self.asm.new_label();
        let bsmall = self.asm.new_label();
        let haveblen = self.asm.new_label();
        self.asm.cbnz(19, pnz);
        self.asm.mov_reg(25, 24);
        self.asm.b(haveblen);
        self.asm.place(pnz);
        self.asm.cmp_reg(24, 19);
        self.asm.b_cond(COND_LS, bsmall);
        self.asm.add_imm(25, 24, 1);
        self.asm.b(haveblen);
        self.asm.place(bsmall);
        self.asm.add_imm(25, 19, 2);
        self.asm.place(haveblen);
        // pad (x27) = max(0, width - (bodylen + signlen))
        let nosl = self.asm.new_label();
        let padok = self.asm.new_label();
        self.asm.mov_reg(8, 25);
        self.asm.cbz(22, nosl);
        self.asm.add_imm(8, 8, 1);
        self.asm.place(nosl);
        self.asm.sub(27, 21, 8);
        self.asm.cmp_imm0(27);
        self.asm.b_cond(COND_GE, padok);
        self.asm.load_imm(27, 0);
        self.asm.place(padok);
        // assemble into outbuf (cursor x26)
        self.asm.adr_global_fs(26, outbuf);
        let do_minus = self.asm.new_label();
        let do_right = self.asm.new_label();
        let fielddone = self.asm.new_label();
        self.fs_tst(20, F_MINUS, do_minus, true);
        self.fs_tst(20, F_ZERO, do_right, false);
        self.fs_float_sign();
        self.fs_float_pad(b'0');
        self.fs_float_body();
        self.asm.b(fielddone);
        self.asm.place(do_right);
        self.fs_float_pad(b' ');
        self.fs_float_sign();
        self.fs_float_body();
        self.asm.b(fielddone);
        self.asm.place(do_minus);
        self.fs_float_sign();
        self.fs_float_body();
        self.fs_float_pad(b' ');
        self.asm.place(fielddone);
        // OutWrite(outbuf, cursor - outbuf)
        let epilogue = self.asm.new_label();
        self.asm.adr_global_fs(1, outbuf);
        self.asm.sub(2, 26, 1);
        let ow = self.fs_routine("OutWrite");
        self.asm.bl(ow);
        self.asm.b(epilogue);
        // inf / NaN: sign then "inf"/"NaN" (Rust's spelling), unpadded. x9=fraction.
        self.asm.place(infnan);
        self.asm.adr_global_fs(26, outbuf);
        self.fs_float_sign();
        let is_nan = self.asm.new_label();
        let wrote = self.asm.new_label();
        self.asm.cbnz(9, is_nan); // fraction nonzero ⇒ NaN
        for ch in [b'i', b'n', b'f'] {
            self.asm.load_imm(8, ch as i64);
            self.asm.store_mem(8, 26, 1);
            self.asm.add_imm(26, 26, 1);
        }
        self.asm.b(wrote);
        self.asm.place(is_nan);
        for ch in [b'N', b'a', b'N'] {
            self.asm.load_imm(8, ch as i64);
            self.asm.store_mem(8, 26, 1);
            self.asm.add_imm(26, 26, 1);
        }
        self.asm.place(wrote);
        self.asm.adr_global_fs(1, outbuf);
        self.asm.sub(2, 26, 1);
        let ow = self.fs_routine("OutWrite");
        self.asm.bl(ow);
        self.asm.place(epilogue);
        for r in [LR, 28, 27, 26, 25, 24, 23, 22, 21, 20, 19] {
            self.asm.pop(r);
        }
        self.asm.ret();
    }

    // Field helpers for `emit_fs_fmt_float`: outbuf cursor x26, sign x22, pad x27,
    // P x19, digit cursor x23, ndig x24.
    fn fs_float_sign(&mut self) {
        let skip = self.asm.new_label();
        self.asm.cbz(22, skip);
        self.asm.store_mem(22, 26, 1);
        self.asm.add_imm(26, 26, 1);
        self.asm.place(skip);
    }
    fn fs_float_pad(&mut self, fill: u8) {
        let top = self.asm.new_label();
        let done = self.asm.new_label();
        self.asm.load_imm(8, fill as i64);
        self.asm.place(top);
        self.asm.cbz(27, done);
        self.asm.store_mem(8, 26, 1);
        self.asm.add_imm(26, 26, 1);
        self.asm.sub_imm(27, 27, 1);
        self.asm.b(top);
        self.asm.place(done);
    }
    /// Copy `x10` digit bytes from `x9` to the cursor `x26`.
    fn fs_float_copy_digits(&mut self) {
        let top = self.asm.new_label();
        let done = self.asm.new_label();
        self.asm.place(top);
        self.asm.cbz(10, done);
        self.asm.load_mem(11, 9, 1, false);
        self.asm.store_mem(11, 26, 1);
        self.asm.add_imm(9, 9, 1);
        self.asm.add_imm(26, 26, 1);
        self.asm.sub_imm(10, 10, 1);
        self.asm.b(top);
        self.asm.place(done);
    }
    fn fs_float_body(&mut self) {
        let p_zero = self.asm.new_label();
        let big = self.asm.new_label();
        let done = self.asm.new_label();
        self.asm.cbz(19, p_zero);
        self.asm.cmp_reg(24, 19);
        self.asm.b_cond(COND_HI, big); // ndig > P
        // ndig <= P: "0", '.', (P-ndig) zeros, ndig digits
        self.asm.load_imm(8, b'0' as i64);
        self.asm.store_mem(8, 26, 1);
        self.asm.add_imm(26, 26, 1);
        self.asm.load_imm(8, b'.' as i64);
        self.asm.store_mem(8, 26, 1);
        self.asm.add_imm(26, 26, 1);
        let zloop = self.asm.new_label();
        let zdone = self.asm.new_label();
        self.asm.sub(12, 19, 24); // P - ndig
        self.asm.place(zloop);
        self.asm.cbz(12, zdone);
        self.asm.load_imm(8, b'0' as i64);
        self.asm.store_mem(8, 26, 1);
        self.asm.add_imm(26, 26, 1);
        self.asm.sub_imm(12, 12, 1);
        self.asm.b(zloop);
        self.asm.place(zdone);
        self.asm.mov_reg(9, 23);
        self.asm.mov_reg(10, 24);
        self.fs_float_copy_digits();
        self.asm.b(done);
        self.asm.place(big); // ndig > P: (ndig-P) ints, '.', P fracs
        self.asm.mov_reg(9, 23);
        self.asm.sub(10, 24, 19);
        self.fs_float_copy_digits();
        self.asm.load_imm(8, b'.' as i64);
        self.asm.store_mem(8, 26, 1);
        self.asm.add_imm(26, 26, 1);
        self.asm.mov_reg(10, 19); // x9 already advanced
        self.fs_float_copy_digits();
        self.asm.b(done);
        self.asm.place(p_zero); // P == 0: all ndig digits
        self.asm.mov_reg(9, 23);
        self.asm.mov_reg(10, 24);
        self.fs_float_copy_digits();
        self.asm.place(done);
    }

    /// `%e`/`%g` scratch BSS (egdig, sigbuf, bodybuf), allocated once.
    fn fs_eg_scratch(&mut self) -> (u64, u64, u64) {
        if let Some(s) = self.fs_eg_off {
            return s;
        }
        let s = (
            self.alloc_bss_fs(1024, 1),
            self.alloc_bss_fs(FS_SIGBUF, 1),
            self.alloc_bss_fs(1024, 1),
        );
        self.fs_eg_off = Some(s);
        s
    }

    /// `%e`/`%g` formatter: `d0`=value, `x0`=P, `x1`=flags, `x2`=width, `x3`=conv
    /// (low 2 bits: 1=`e`, 2=`g`; bit 2 = uppercase). Works from the value's exact
    /// finite decimal expansion (`Dint = m·5^(−e)`), rounds to N significant
    /// figures, then renders `e`-style or `f`-style. Persistent state in x19..x28.
    fn emit_fs_fmt_float_eg(&mut self) {
        let (bn, _) = self.fs_float_scratch();
        let (egdig, sigbuf, bodybuf) = self.fs_eg_scratch();
        let egend = egdig + 1024;
        let outbuf = self.fs_scratch()[1];
        for r in [19, 20, 21, 22, 23, 24, 25, 26, 27, 28, LR] {
            self.asm.push(r);
        }
        self.asm.mov_reg(19, 0); // P
        self.asm.mov_reg(20, 1); // flags
        self.asm.mov_reg(21, 2); // width
        self.asm.mov_reg(24, 3); // conv
        self.asm.fmov_to_gpr(23, 0); // value bits
        // sign (x22) + magnitude bits (x23)
        let sgpos = self.asm.new_label();
        let shavesign = self.asm.new_label();
        let snospace = self.asm.new_label();
        self.asm.load_imm(8, 0x7FFF_FFFF_FFFF_FFFF);
        self.asm.and(9, 23, 8);
        self.asm.load_imm(22, 0);
        self.asm.cbz(9, sgpos);
        self.asm.lsr_imm(10, 23, 63);
        self.asm.cbz(10, sgpos);
        self.asm.load_imm(22, b'-' as i64);
        self.asm.b(shavesign);
        self.asm.place(sgpos);
        self.fs_tst(20, F_PLUS, shavesign, false);
        self.asm.load_imm(22, b'+' as i64);
        self.asm.place(shavesign);
        self.asm.cbnz(22, snospace);
        self.fs_tst(20, F_SPACE, snospace, false);
        self.asm.load_imm(22, b' ' as i64);
        self.asm.place(snospace);
        self.asm.mov_reg(23, 9); // magnitude bits
        // decompose → exp (x12), frac/mantissa (x9), e2 (x12)
        let eg_infnan = self.asm.new_label();
        let eg_zero = self.asm.new_label();
        let eg_subn = self.asm.new_label();
        let eg_havem = self.asm.new_label();
        self.asm.lsr_imm(12, 23, 52);
        self.asm.load_imm(8, 0x7FF);
        self.asm.and(12, 12, 8);
        self.asm.load_imm(8, 0x000F_FFFF_FFFF_FFFF);
        self.asm.and(9, 23, 8); // frac
        self.asm.cmp_imm(12, 0x7FF);
        self.asm.b_cond(COND_EQ, eg_infnan);
        self.asm.cbz(23, eg_zero); // value == 0
        self.asm.cbz(12, eg_subn);
        self.asm.load_imm(8, 0x0010_0000_0000_0000);
        self.asm.orr(9, 9, 8);
        self.asm.sub_imm(12, 12, 1075);
        self.asm.b(eg_havem);
        self.asm.place(eg_subn);
        self.asm.load_imm(12, -1074);
        self.asm.place(eg_havem);
        // BIGNUM = m
        let ezloop = self.asm.new_label();
        self.asm.adr_global_fs(8, bn);
        self.asm.load_imm(13, 0);
        self.asm.load_imm(14, 0);
        self.asm.place(ezloop);
        self.asm.str_idx8(14, 8, 13);
        self.asm.add_imm(13, 13, 1);
        self.asm.cmp_imm(13, NLIMBS as u32);
        self.asm.b_cond(COND_LO, ezloop);
        self.asm.store_mem(9, 8, 8); // BIGNUM[0] = m
        // pe = min(e2,0); Dint = m·2^e2 (e2≥0) or m·5^(−e2) (e2<0).
        let eg_neg = self.asm.new_label();
        let eg_mulloop = self.asm.new_label();
        let eg_built = self.asm.new_label();
        self.asm.cmp_imm0(12);
        self.asm.b_cond(COND_LT, eg_neg);
        self.asm.load_imm(25, 0); // pe = 0
        self.asm.mov_reg(0, 12);
        let sl = self.fs_routine("BnShl");
        self.asm.bl(sl);
        self.asm.b(eg_built);
        self.asm.place(eg_neg);
        self.asm.mov_reg(25, 12); // pe = e2
        self.asm.neg(26, 12); // count = -e2 (x26, becomes X later)
        self.asm.place(eg_mulloop);
        self.asm.cbz(26, eg_built);
        self.asm.load_imm(0, 5);
        let bm = self.fs_routine("BnMul");
        self.asm.bl(bm);
        self.asm.sub_imm(26, 26, 1);
        self.asm.b(eg_mulloop);
        self.asm.place(eg_built);
        // extract all digits of Dint into egdig (downward); x23 = MSB ptr
        let eg_dloop = self.asm.new_label();
        let eg_zchk = self.asm.new_label();
        let eg_notz = self.asm.new_label();
        let eg_done = self.asm.new_label();
        self.asm.adr_global_fs(23, egend);
        self.asm.place(eg_dloop);
        let bd = self.fs_routine("BnDiv10");
        self.asm.bl(bd);
        self.asm.add_imm(0, 0, b'0' as u32);
        self.asm.sub_imm(23, 23, 1);
        self.asm.store_mem(0, 23, 1);
        self.asm.adr_global_fs(8, bn);
        self.asm.load_imm(9, 0);
        self.asm.place(eg_zchk);
        self.asm.ldr_idx8(10, 8, 9);
        self.asm.cbnz(10, eg_notz);
        self.asm.add_imm(9, 9, 1);
        self.asm.cmp_imm(9, NLIMBS as u32);
        self.asm.b_cond(COND_LO, eg_zchk);
        self.asm.b(eg_done);
        self.asm.place(eg_notz);
        self.asm.b(eg_dloop);
        self.asm.place(eg_done);
        // ndig (x28) = egend − x23; X (x26) = ndig − 1 + pe
        self.asm.adr_global_fs(8, egend);
        self.asm.sub(28, 8, 23);
        self.asm.add(26, 28, 25);
        self.asm.sub_imm(26, 26, 1);
        self.eg_nsig(); // nsig → x27 from conv (x24) and P (x19)
        // round to nsig significant figures (real → x11)
        let eg_noround = self.asm.new_label();
        let eg_haveround = self.asm.new_label();
        self.asm.cmp_reg(28, 27);
        self.asm.b_cond(COND_LS, eg_noround);
        self.eg_round();
        self.asm.mov_reg(11, 27); // real = nsig
        self.asm.b(eg_haveround);
        self.asm.place(eg_noround);
        self.asm.mov_reg(11, 28); // real = ndig
        self.asm.place(eg_haveround);
        self.eg_copy_to_sigbuf(sigbuf); // real digits + '0' pad → sigbuf; x23 = base
        self.eg_format(bodybuf);
        self.eg_field(bodybuf, outbuf);
        let eg_epilogue = self.asm.new_label();
        self.asm.b(eg_epilogue);
        // value == 0
        self.asm.place(eg_zero);
        self.asm.load_imm(26, 0); // X = 0
        self.eg_nsig();
        let z2 = self.asm.new_label();
        let z2d = self.asm.new_label();
        self.asm.adr_global_fs(8, sigbuf);
        self.asm.load_imm(9, 0);
        self.asm.place(z2);
        self.asm.cmp_reg(9, 27);
        self.asm.b_cond(COND_HS, z2d);
        self.asm.load_imm(10, b'0' as i64);
        self.asm.store_mem(10, 8, 1);
        self.asm.add_imm(8, 8, 1);
        self.asm.add_imm(9, 9, 1);
        self.asm.b(z2);
        self.asm.place(z2d);
        self.asm.adr_global_fs(23, sigbuf);
        self.eg_format(bodybuf);
        self.eg_field(bodybuf, outbuf);
        self.asm.b(eg_epilogue);
        // inf / NaN (x9 = fraction)
        self.asm.place(eg_infnan);
        self.asm.adr_global_fs(26, outbuf); // cursor
        self.fs_float_sign();
        let eg_nan = self.asm.new_label();
        let eg_wrote = self.asm.new_label();
        self.asm.cbnz(9, eg_nan);
        for ch in [b'i', b'n', b'f'] {
            self.asm.load_imm(8, ch as i64);
            self.asm.store_mem(8, 26, 1);
            self.asm.add_imm(26, 26, 1);
        }
        self.asm.b(eg_wrote);
        self.asm.place(eg_nan);
        for ch in [b'N', b'a', b'N'] {
            self.asm.load_imm(8, ch as i64);
            self.asm.store_mem(8, 26, 1);
            self.asm.add_imm(26, 26, 1);
        }
        self.asm.place(eg_wrote);
        self.asm.adr_global_fs(1, outbuf);
        self.asm.sub(2, 26, 1);
        let ow = self.fs_routine("OutWrite");
        self.asm.bl(ow);
        self.asm.place(eg_epilogue);
        for r in [LR, 28, 27, 26, 25, 24, 23, 22, 21, 20, 19] {
            self.asm.pop(r);
        }
        self.asm.ret();
    }

    /// nsig (x27): `e` → P+1; `g` → max(P,1). Reads conv (x24), P (x19).
    fn eg_nsig(&mut self) {
        let isg = self.asm.new_label();
        let done = self.asm.new_label();
        self.asm.and_imm_lowbits(8, 24, 2); // conv & 3
        self.asm.cmp_imm(8, 2);
        self.asm.b_cond(COND_EQ, isg);
        self.asm.add_imm(27, 19, 1); // P+1
        self.asm.b(done);
        self.asm.place(isg);
        self.asm.mov_reg(27, 19); // max(P,1)
        self.asm.cbnz(27, done);
        self.asm.load_imm(27, 1);
        self.asm.place(done);
    }

    /// Round egdig[x23..] (ndig=x28) to nsig (x27) significant figures, half-even;
    /// may bump X (x26).
    fn eg_round(&mut self) {
        let sloop = self.asm.new_label();
        let nstick = self.asm.new_label();
        let sdone = self.asm.new_label();
        let roundup = self.asm.new_label();
        let noup = self.asm.new_label();
        let iloop = self.asm.new_label();
        let carry = self.asm.new_label();
        let overflow = self.asm.new_label();
        let done = self.asm.new_label();
        self.asm.add(8, 23, 27); // &cursor[nsig]
        self.asm.load_mem(9, 8, 1, false); // round digit
        self.asm.load_imm(11, 0); // sticky
        self.asm.add_imm(12, 8, 1);
        self.asm.add(13, 23, 28); // end = cursor + ndig
        self.asm.place(sloop);
        self.asm.cmp_reg(12, 13);
        self.asm.b_cond(COND_HS, sdone);
        self.asm.load_mem(14, 12, 1, false);
        self.asm.cmp_imm(14, b'0' as u32);
        self.asm.b_cond(COND_EQ, nstick);
        self.asm.load_imm(11, 1);
        self.asm.b(sdone);
        self.asm.place(nstick);
        self.asm.add_imm(12, 12, 1);
        self.asm.b(sloop);
        self.asm.place(sdone);
        self.asm.cmp_imm(9, b'5' as u32);
        self.asm.b_cond(COND_GT, roundup);
        self.asm.b_cond(COND_LT, noup);
        self.asm.cbnz(11, roundup); // sticky
        self.asm.add(8, 23, 27);
        self.asm.sub_imm(8, 8, 1);
        self.asm.load_mem(12, 8, 1, false);
        self.asm.and_imm_lowbits(12, 12, 1);
        self.asm.cbz(12, noup); // last-kept even
        self.asm.place(roundup);
        self.asm.sub_imm(12, 27, 1); // index = nsig-1
        self.asm.place(iloop);
        self.asm.add(8, 23, 12);
        self.asm.load_mem(9, 8, 1, false);
        self.asm.cmp_imm(9, b'9' as u32);
        self.asm.b_cond(COND_EQ, carry);
        self.asm.add_imm(9, 9, 1);
        self.asm.store_mem(9, 8, 1);
        self.asm.b(done);
        self.asm.place(carry);
        self.asm.load_imm(9, b'0' as i64);
        self.asm.store_mem(9, 8, 1);
        self.asm.sub_imm(12, 12, 1);
        self.asm.cmp_imm0(12);
        self.asm.b_cond(COND_LT, overflow);
        self.asm.b(iloop);
        self.asm.place(overflow);
        self.asm.load_imm(9, b'1' as i64);
        self.asm.store_mem(9, 23, 1);
        self.asm.add_imm(26, 26, 1); // X += 1
        self.asm.place(noup);
        self.asm.place(done);
    }

    /// Copy `real` (x11) digits from x23, then '0'-pad to nsig (x27), into sigbuf;
    /// leaves x23 = sigbuf base.
    fn eg_copy_to_sigbuf(&mut self, sigbuf: u64) {
        let loop_l = self.asm.new_label();
        let zfill = self.asm.new_label();
        let put = self.asm.new_label();
        let done = self.asm.new_label();
        self.asm.adr_global_fs(8, sigbuf); // dst
        self.asm.mov_reg(9, 23); // src
        self.asm.load_imm(12, 0); // i
        self.asm.place(loop_l);
        self.asm.cmp_reg(12, 27);
        self.asm.b_cond(COND_HS, done);
        self.asm.cmp_reg(12, 11);
        self.asm.b_cond(COND_HS, zfill);
        self.asm.load_mem(13, 9, 1, false);
        self.asm.add_imm(9, 9, 1);
        self.asm.b(put);
        self.asm.place(zfill);
        self.asm.load_imm(13, b'0' as i64);
        self.asm.place(put);
        self.asm.store_mem(13, 8, 1);
        self.asm.add_imm(8, 8, 1);
        self.asm.add_imm(12, 12, 1);
        self.asm.b(loop_l);
        self.asm.place(done);
        self.asm.adr_global_fs(23, sigbuf);
    }

    /// Build the body into bodybuf (body cursor → x10), choosing `e`-style or
    /// `f`-style and trimming trailing zeros (unless `#`). x23=sigbuf, X=x26,
    /// nsig=x27, P=x19, conv=x24.
    fn eg_format(&mut self, bodybuf: u64) {
        let kind_g = self.asm.new_label();
        let use_e = self.asm.new_label();
        let trim = self.asm.new_label();
        let no_trim = self.asm.new_label();
        self.asm.adr_global_fs(10, bodybuf);
        self.asm.and_imm_lowbits(8, 24, 2);
        self.asm.cmp_imm(8, 2);
        self.asm.b_cond(COND_EQ, kind_g);
        self.eg_body_sci(19); // %e: precision P
        self.asm.b(no_trim);
        self.asm.place(kind_g);
        self.asm.load_imm(8, -4);
        self.asm.cmp_reg(26, 8);
        self.asm.b_cond(COND_LT, use_e); // X < -4
        self.asm.cmp_reg(26, 27);
        self.asm.b_cond(COND_GE, use_e); // X >= p
        self.eg_body_fixed();
        self.asm.b(trim);
        self.asm.place(use_e);
        self.asm.sub_imm(15, 27, 1); // p-1
        self.eg_body_sci(15);
        self.asm.place(trim);
        self.fs_tst(20, F_HASH, no_trim, true);
        self.eg_trim(bodybuf);
        self.asm.place(no_trim);
    }

    /// `e`-style body: `d.ddde±XX` with `prec_reg` fractional digits.
    fn eg_body_sci(&mut self, prec_reg: u32) {
        self.asm.mov_reg(14, prec_reg); // precision (stable)
        self.asm.load_mem(8, 23, 1, false); // first digit
        self.asm.store_mem(8, 10, 1);
        self.asm.add_imm(10, 10, 1);
        let nofrac = self.asm.new_label();
        let floop = self.asm.new_label();
        let fdone = self.asm.new_label();
        self.asm.cbz(14, nofrac);
        self.asm.load_imm(8, b'.' as i64);
        self.asm.store_mem(8, 10, 1);
        self.asm.add_imm(10, 10, 1);
        self.asm.add_imm(9, 23, 1); // &sigbuf[1]
        self.asm.place(floop);
        self.asm.cbz(14, fdone);
        self.asm.load_mem(8, 9, 1, false);
        self.asm.store_mem(8, 10, 1);
        self.asm.add_imm(10, 10, 1);
        self.asm.add_imm(9, 9, 1);
        self.asm.sub_imm(14, 14, 1);
        self.asm.b(floop);
        self.asm.place(fdone);
        self.asm.place(nofrac);
        // 'e' / 'E'
        let lower = self.asm.new_label();
        let wrote = self.asm.new_label();
        self.fs_tst(24, 4, lower, false);
        self.asm.load_imm(8, b'E' as i64);
        self.asm.b(wrote);
        self.asm.place(lower);
        self.asm.load_imm(8, b'e' as i64);
        self.asm.place(wrote);
        self.asm.store_mem(8, 10, 1);
        self.asm.add_imm(10, 10, 1);
        // exponent sign + |X| (≥ 2 digits)
        let xneg = self.asm.new_label();
        let haveabs = self.asm.new_label();
        self.asm.cmp_imm0(26);
        self.asm.b_cond(COND_GE, xneg); // reuse label name; ≥0 path
        self.asm.load_imm(8, b'-' as i64);
        self.asm.store_mem(8, 10, 1);
        self.asm.add_imm(10, 10, 1);
        self.asm.neg(9, 26);
        self.asm.b(haveabs);
        self.asm.place(xneg);
        self.asm.load_imm(8, b'+' as i64);
        self.asm.store_mem(8, 10, 1);
        self.asm.add_imm(10, 10, 1);
        self.asm.mov_reg(9, 26);
        self.asm.place(haveabs);
        // x9 = |X|; hundreds (if any), tens, ones
        let nohund = self.asm.new_label();
        self.asm.load_imm(12, 100);
        self.asm.udiv(13, 9, 12);
        self.asm.msub(9, 13, 12, 9); // |X| % 100
        self.asm.cbz(13, nohund);
        self.asm.add_imm(13, 13, b'0' as u32);
        self.asm.store_mem(13, 10, 1);
        self.asm.add_imm(10, 10, 1);
        self.asm.place(nohund);
        self.asm.load_imm(12, 10);
        self.asm.udiv(13, 9, 12);
        self.asm.msub(14, 13, 12, 9);
        self.asm.add_imm(13, 13, b'0' as u32);
        self.asm.store_mem(13, 10, 1);
        self.asm.add_imm(10, 10, 1);
        self.asm.add_imm(14, 14, b'0' as u32);
        self.asm.store_mem(14, 10, 1);
        self.asm.add_imm(10, 10, 1);
    }

    /// `f`-style body from the sigbuf digits, point placed by X (x26), nsig (x27).
    fn eg_body_fixed(&mut self) {
        let xneg = self.asm.new_label();
        let done = self.asm.new_label();
        self.asm.cmp_imm0(26);
        self.asm.b_cond(COND_LT, xneg);
        // X ≥ 0: int = sigbuf[0..X+1]
        let iloop = self.asm.new_label();
        let idone = self.asm.new_label();
        self.asm.mov_reg(8, 23); // src
        self.asm.add_imm(9, 26, 1); // X+1
        self.asm.place(iloop);
        self.asm.cbz(9, idone);
        self.asm.load_mem(13, 8, 1, false);
        self.asm.store_mem(13, 10, 1);
        self.asm.add_imm(10, 10, 1);
        self.asm.add_imm(8, 8, 1);
        self.asm.sub_imm(9, 9, 1);
        self.asm.b(iloop);
        self.asm.place(idone);
        // frac: nsig - X - 1 digits, if any
        let nofrac = self.asm.new_label();
        let floop = self.asm.new_label();
        let fdone = self.asm.new_label();
        self.asm.sub(9, 27, 26);
        self.asm.sub_imm(9, 9, 1); // nsig - X - 1
        self.asm.cmp_imm0(9);
        self.asm.b_cond(COND_LE, nofrac);
        self.asm.load_imm(13, b'.' as i64);
        self.asm.store_mem(13, 10, 1);
        self.asm.add_imm(10, 10, 1);
        self.asm.place(floop);
        self.asm.cbz(9, fdone);
        self.asm.load_mem(13, 8, 1, false);
        self.asm.store_mem(13, 10, 1);
        self.asm.add_imm(10, 10, 1);
        self.asm.add_imm(8, 8, 1);
        self.asm.sub_imm(9, 9, 1);
        self.asm.b(floop);
        self.asm.place(fdone);
        self.asm.place(nofrac);
        self.asm.b(done);
        // X < 0: "0." then (−X−1) zeros then all nsig digits
        self.asm.place(xneg);
        self.asm.load_imm(13, b'0' as i64);
        self.asm.store_mem(13, 10, 1);
        self.asm.add_imm(10, 10, 1);
        self.asm.load_imm(13, b'.' as i64);
        self.asm.store_mem(13, 10, 1);
        self.asm.add_imm(10, 10, 1);
        let zloop = self.asm.new_label();
        let zdone = self.asm.new_label();
        self.asm.neg(9, 26);
        self.asm.sub_imm(9, 9, 1); // -X-1
        self.asm.place(zloop);
        self.asm.cbz(9, zdone);
        self.asm.load_imm(13, b'0' as i64);
        self.asm.store_mem(13, 10, 1);
        self.asm.add_imm(10, 10, 1);
        self.asm.sub_imm(9, 9, 1);
        self.asm.b(zloop);
        self.asm.place(zdone);
        let dloop = self.asm.new_label();
        let ddone = self.asm.new_label();
        self.asm.mov_reg(8, 23);
        self.asm.mov_reg(9, 27); // nsig
        self.asm.place(dloop);
        self.asm.cbz(9, ddone);
        self.asm.load_mem(13, 8, 1, false);
        self.asm.store_mem(13, 10, 1);
        self.asm.add_imm(10, 10, 1);
        self.asm.add_imm(8, 8, 1);
        self.asm.sub_imm(9, 9, 1);
        self.asm.b(dloop);
        self.asm.place(ddone);
        self.asm.place(done);
    }

    /// Trim trailing zeros (and a bare `.`) from the mantissa of the body in
    /// bodybuf, preserving any `e±XX` exponent. Body end is x10.
    fn eg_trim(&mut self, bodybuf: u64) {
        let scan = self.asm.new_label();
        let found_e = self.asm.new_label();
        let no_e = self.asm.new_label();
        let have_split = self.asm.new_label();
        self.asm.adr_global_fs(8, bodybuf); // p
        self.asm.place(scan);
        self.asm.cmp_reg(8, 10);
        self.asm.b_cond(COND_HS, no_e);
        self.asm.load_mem(12, 8, 1, false);
        self.asm.cmp_imm(12, b'e' as u32);
        self.asm.b_cond(COND_EQ, found_e);
        self.asm.cmp_imm(12, b'E' as u32);
        self.asm.b_cond(COND_EQ, found_e);
        self.asm.add_imm(8, 8, 1);
        self.asm.b(scan);
        self.asm.place(no_e);
        self.asm.mov_reg(13, 10); // mantissa end = body end
        self.asm.load_imm(14, 0); // no exponent
        self.asm.b(have_split);
        self.asm.place(found_e);
        self.asm.mov_reg(13, 8); // mantissa end = 'e'
        self.asm.sub(14, 10, 8); // exponent length
        self.asm.place(have_split);
        // only trim if the mantissa contains '.'
        let mscan = self.asm.new_label();
        let has_dot = self.asm.new_label();
        let nodot = self.asm.new_label();
        self.asm.adr_global_fs(8, bodybuf);
        self.asm.place(mscan);
        self.asm.cmp_reg(8, 13);
        self.asm.b_cond(COND_HS, nodot);
        self.asm.load_mem(12, 8, 1, false);
        self.asm.cmp_imm(12, b'.' as u32);
        self.asm.b_cond(COND_EQ, has_dot);
        self.asm.add_imm(8, 8, 1);
        self.asm.b(mscan);
        self.asm.place(has_dot);
        // strip trailing '0', then a trailing '.', from [bodybuf .. x13)
        let tloop = self.asm.new_label();
        let tdone = self.asm.new_label();
        let nodot2 = self.asm.new_label();
        self.asm.place(tloop);
        self.asm.sub_imm(8, 13, 1);
        self.asm.load_mem(12, 8, 1, false);
        self.asm.cmp_imm(12, b'0' as u32);
        self.asm.b_cond(COND_NE, tdone);
        self.asm.sub_imm(13, 13, 1);
        self.asm.b(tloop);
        self.asm.place(tdone);
        self.asm.sub_imm(8, 13, 1);
        self.asm.load_mem(12, 8, 1, false);
        self.asm.cmp_imm(12, b'.' as u32);
        self.asm.b_cond(COND_NE, nodot2);
        self.asm.sub_imm(13, 13, 1);
        self.asm.place(nodot2);
        // move the exponent suffix down to x13, set x10 = new end
        let eloop = self.asm.new_label();
        let edone = self.asm.new_label();
        self.asm.sub(9, 10, 14); // exponent source start = old end - explen
        self.asm.place(eloop);
        self.asm.cbz(14, edone);
        self.asm.load_mem(12, 9, 1, false);
        self.asm.store_mem(12, 13, 1);
        self.asm.add_imm(13, 13, 1);
        self.asm.add_imm(9, 9, 1);
        self.asm.sub_imm(14, 14, 1);
        self.asm.b(eloop);
        self.asm.place(edone);
        self.asm.mov_reg(10, 13); // new body end
        self.asm.place(nodot);
    }

    /// Render the body field (sign + pad + bodybuf) into outbuf and write it. Body
    /// end is x10; reuses the `%f` field helpers (cursor x26, sign x22, pad x27).
    fn eg_field(&mut self, bodybuf: u64, outbuf: u64) {
        // bodylen (x11) = x10 − bodybuf
        self.asm.adr_global_fs(8, bodybuf);
        self.asm.sub(11, 10, 8);
        // pad (x27) = max(0, width − (bodylen + signlen))
        let nosl = self.asm.new_label();
        let padok = self.asm.new_label();
        self.asm.mov_reg(8, 11);
        self.asm.cbz(22, nosl);
        self.asm.add_imm(8, 8, 1);
        self.asm.place(nosl);
        self.asm.sub(27, 21, 8);
        self.asm.cmp_imm0(27);
        self.asm.b_cond(COND_GE, padok);
        self.asm.load_imm(27, 0);
        self.asm.place(padok);
        // assemble into outbuf (cursor x26)
        self.asm.adr_global_fs(26, outbuf);
        let do_minus = self.asm.new_label();
        let do_right = self.asm.new_label();
        let fielddone = self.asm.new_label();
        self.fs_tst(20, F_MINUS, do_minus, true);
        self.fs_tst(20, F_ZERO, do_right, false);
        self.fs_float_sign();
        self.fs_float_pad(b'0');
        self.eg_copy_body(bodybuf);
        self.asm.b(fielddone);
        self.asm.place(do_right);
        self.fs_float_pad(b' ');
        self.fs_float_sign();
        self.eg_copy_body(bodybuf);
        self.asm.b(fielddone);
        self.asm.place(do_minus);
        self.fs_float_sign();
        self.eg_copy_body(bodybuf);
        self.fs_float_pad(b' ');
        self.asm.place(fielddone);
        self.asm.adr_global_fs(1, outbuf);
        self.asm.sub(2, 26, 1);
        let ow = self.fs_routine("OutWrite");
        self.asm.bl(ow);
    }
    /// Copy `x11` body bytes from bodybuf to the cursor x26.
    fn eg_copy_body(&mut self, bodybuf: u64) {
        let top = self.asm.new_label();
        let done = self.asm.new_label();
        self.asm.adr_global_fs(9, bodybuf); // src
        self.asm.mov_reg(12, 11); // count
        self.asm.place(top);
        self.asm.cbz(12, done);
        self.asm.load_mem(13, 9, 1, false);
        self.asm.store_mem(13, 26, 1);
        self.asm.add_imm(9, 9, 1);
        self.asm.add_imm(26, 26, 1);
        self.asm.sub_imm(12, 12, 1);
        self.asm.b(top);
        self.asm.place(done);
    }

    /// `MemCmp(x0=a, x1=b, x2=n) -> x0` — a sign in {-1, 0, 1}.
    fn emit_fs_memcmp(&mut self) {
        let l = self.asm.new_label();
        let diff = self.asm.new_label();
        let eq = self.asm.new_label();
        let done = self.asm.new_label();
        self.asm.place(l);
        self.asm.cbz(2, eq);
        self.asm.load_mem(9, 0, 1, false);
        self.asm.load_mem(10, 1, 1, false);
        self.asm.cmp_reg(9, 10);
        self.asm.b_cond(COND_NE, diff);
        self.asm.add_imm(0, 0, 1);
        self.asm.add_imm(1, 1, 1);
        self.asm.sub_imm(2, 2, 1);
        self.asm.b(l);
        self.asm.place(diff);
        self.asm.load_imm(0, 1);
        self.asm.cmp_reg(9, 10);
        self.asm.b_cond(COND_HI, done);
        self.asm.load_imm(0, -1);
        self.asm.place(done);
        self.asm.ret();
        self.asm.place(eq);
        self.asm.load_imm(0, 0);
        self.asm.ret();
    }

    /// `StrPrint(dst, fmt, ...)` / `CatPrint(dst, fmt, ...)` -> `sprintf` into
    /// `dst` (or `dst + strlen(dst)` when `append`), returning `dst`.
    fn gen_formatted_write(
        &mut self,
        args: &[Expr],
        pos: Pos,
        append: bool,
    ) -> Result<(), CodegenError> {
        let what = if append { "CatPrint" } else { "StrPrint" };
        let (dst, rest) = args
            .split_first()
            .ok_or_else(|| CodegenError::at(pos, format!("{what} requires a destination")))?;
        let (fmt, rest) = match rest.split_first() {
            Some((first, rest)) => match &first.kind {
                ExprKind::Str(s) => (s.clone(), rest),
                _ => {
                    return Err(CodegenError::at(
                        pos,
                        format!("arm64 backend: {what}'s format must be a string literal"),
                    ));
                }
            },
            None => {
                return Err(CodegenError::at(
                    pos,
                    format!("{what} requires a format string"),
                ));
            }
        };

        if self.freestanding {
            return self.gen_formatted_write_fs(dst, &fmt, rest, append);
        }

        // Evaluate dst and stash it in a frame slot (it survives the SP-relative
        // variadic area and becomes the result).
        self.gen_expr(dst)?;
        let dst_off = self.alloc(8, 8);
        self.asm.sub_imm(T2, FP, dst_off);
        self.gen_store(RES, T2, &Type::I64);

        // Compute the sprintf target: dst, or dst + strlen(dst) for an append.
        let target_off = self.alloc(8, 8);
        if append {
            self.load_local(0, dst_off, &Type::I64); // x0 = dst
            self.asm.bl_extern("_strlen"); // x0 = strlen(dst)
            self.load_local(T2, dst_off, &Type::I64); // T2 = dst
            self.asm.add(T2, T2, 0); // T2 = dst + len
            self.asm.sub_imm(SCRATCH, FP, target_off);
            self.gen_store(T2, SCRATCH, &Type::I64);
        } else {
            self.load_local(RES, dst_off, &Type::I64);
            self.asm.sub_imm(T2, FP, target_off);
            self.gen_store(RES, T2, &Type::I64);
        }

        let c_fmt = translate_format(&fmt)?;
        let fmt_idx = self.asm.intern_string(&c_fmt);
        let sz = self.pass_variadics(&rest, 2)?; // sprintf(dst, fmt, ...): variadics from x2
        self.load_local(0, target_off, &Type::I64); // x0 = target
        self.asm.adr(1, fmt_idx); // x1 = format
        self.asm.bl_extern("_sprintf");
        if sz > 0 {
            self.asm.add_sp_imm(sz);
        }
        self.load_local(RES, dst_off, &Type::I64); // return dst
        Ok(())
    }

    /// `MStrPrint(fmt, ...)` -> format into a fresh, right-sized buffer: measure
    /// with `snprintf(NULL, 0, ...)`, `malloc(len + 1)`, then `sprintf`. Returns
    /// the new buffer. The variadic args stay on the stack across both calls.
    fn gen_mstrprint(&mut self, args: &[Expr], pos: Pos) -> Result<(), CodegenError> {
        let (fmt, rest) = match args.split_first() {
            Some((first, rest)) => match &first.kind {
                ExprKind::Str(s) => (s.clone(), rest),
                _ => {
                    return Err(CodegenError::at(
                        pos,
                        "arm64 backend: MStrPrint's format must be a string literal",
                    ));
                }
            },
            None => return Err(CodegenError::at(pos, "MStrPrint requires a format string")),
        };

        let buf_off = self.alloc(8, 8);
        let c_fmt = translate_format(&fmt)?;
        let fmt_idx = self.asm.intern_string(&c_fmt);
        let k = rest.len() as u32;
        let varsize = align16(k * 8);
        if varsize > 0 {
            self.asm.sub_sp_imm(varsize);
        }
        for (i, arg) in rest.iter().enumerate() {
            if is_f64(&self.expr_ty(arg)) {
                self.gen_fexpr(arg)?;
                self.asm.fmov_to_gpr(RES, FRES);
            } else {
                self.gen_expr(arg)?;
            }
            self.asm.str_sp(RES, i as u32 * 8);
        }
        // snprintf(NULL, 0, fmt, ...) -> required length (an int in w0).
        self.load_variadic_regs(rest, 0, 3); // variadics from x3 (NULL, 0, fmt named)
        self.asm.load_imm(0, 0); // x0 = NULL
        self.asm.load_imm(1, 0); // x1 = 0
        self.asm.adr(2, fmt_idx); // x2 = format
        self.asm.bl_extern("_snprintf");
        self.asm.ubfm(0, 0, 0, 31); // x0 = (u32) len
        self.asm.add_imm(0, 0, 1); // + 1 for the NUL
        self.asm.bl_extern("_malloc"); // x0 = buf
        self.asm.sub_imm(T2, FP, buf_off);
        self.gen_store(0, T2, &Type::I64); // save buf
        // sprintf(buf, fmt, ...) reads the same variadic args still on the stack
        // (reloaded into registers for Linux, since the calls above clobbered them).
        self.load_local(0, buf_off, &Type::I64); // x0 = buf
        self.asm.adr(1, fmt_idx); // x1 = format
        self.load_variadic_regs(rest, 0, 2); // variadics from x2
        self.asm.bl_extern("_sprintf");
        if varsize > 0 {
            self.asm.add_sp_imm(varsize);
        }
        self.load_local(RES, buf_off, &Type::I64); // return buf
        Ok(())
    }

    /// `I64ToStr(n, buf)` / `F64ToStr(f, buf)` -> `sprintf(buf, fmt, value)`,
    /// returning `buf`. `fmt` is a fixed single-conversion format.
    fn gen_tostr(
        &mut self,
        value: &Expr,
        buf: &Expr,
        fmt: &str,
        is_float: bool,
        _pos: Pos,
    ) -> Result<(), CodegenError> {
        self.gen_expr(buf)?; // RES = buf
        let buf_off = self.alloc(8, 8);
        self.asm.sub_imm(T2, FP, buf_off);
        self.gen_store(RES, T2, &Type::I64);

        let c_fmt = translate_format(fmt)?;
        let fmt_idx = self.asm.intern_string(&c_fmt);
        self.asm.sub_sp_imm(16); // one 16-aligned variadic slot
        if is_float {
            self.gen_fexpr(value)?;
            self.asm.fmov_to_gpr(RES, FRES);
        } else {
            self.gen_expr(value)?;
        }
        self.asm.str_sp(RES, 0);
        self.load_local(0, buf_off, &Type::I64); // x0 = buf
        self.asm.adr(1, fmt_idx); // x1 = format
        self.load_variadic_regs(std::slice::from_ref(value), 0, 2); // the value from x2/v0
        self.asm.bl_extern("_sprintf");
        self.asm.add_sp_imm(16);
        self.load_local(RES, buf_off, &Type::I64); // return buf
        Ok(())
    }

    /// An `Is*` ctype predicate: leave 1 in RES if the argument byte falls in any
    /// of `ranges` (inclusive), else 0. Each range is the unsigned check
    /// `(c - lo) <= (hi - lo)`; the first hit short-circuits.
    fn gen_ctype(&mut self, arg: &Expr, ranges: &[(u8, u8)]) -> Result<(), CodegenError> {
        self.gen_expr(arg)?; // RES = c
        let done = self.asm.new_label();
        for &(lo, hi) in ranges {
            let skip = self.asm.new_label();
            self.asm.sub_imm(T2, RES, lo as u32); // T2 = c - lo
            self.asm.cmp_imm(T2, (hi - lo) as u32);
            self.asm.b_cond(COND_HI, skip); // (c - lo) > (hi - lo) unsigned -> miss
            self.asm.load_imm(RES, 1);
            self.asm.b(done);
            self.asm.place(skip);
        }
        self.asm.load_imm(RES, 0);
        self.asm.place(done);
        Ok(())
    }

    /// `StrToUpper(str)` / `StrToLower(str)` — walk `str` to its NUL, replacing
    /// each byte with `toupper`/`tolower` of it; return `str`. The cursor lives in
    /// a frame slot since the per-char libc call clobbers the temp registers.
    fn gen_str_case(&mut self, arg: &Expr, upper: bool) -> Result<(), CodegenError> {
        self.gen_expr(arg)?; // RES = str
        let str_off = self.alloc(8, 8);
        self.asm.sub_imm(T2, FP, str_off);
        self.gen_store(RES, T2, &Type::I64); // save str (the result)
        let cur_off = self.alloc(8, 8);
        self.asm.sub_imm(T2, FP, cur_off);
        self.gen_store(RES, T2, &Type::I64); // cursor = str

        let l_loop = self.asm.new_label();
        let l_end = self.asm.new_label();
        self.asm.place(l_loop);
        self.load_local(T2, cur_off, &Type::I64); // T2 = cursor
        self.asm.load_mem(RES, T2, 1, false); // RES = *cursor
        self.asm.cbz(RES, l_end); // NUL -> done
        self.asm.mov_reg(0, RES); // x0 = char
        self.asm
            .bl_extern(if upper { "_toupper" } else { "_tolower" });
        self.load_local(T2, cur_off, &Type::I64); // reload cursor (call clobbered it)
        self.asm.store_mem(0, T2, 1); // *cursor = result byte
        self.asm.add_imm(T2, T2, 1);
        self.asm.sub_imm(SCRATCH, FP, cur_off);
        self.gen_store(T2, SCRATCH, &Type::I64); // cursor++
        self.asm.b(l_loop);
        self.asm.place(l_end);
        self.load_local(RES, str_off, &Type::I64); // return str
        Ok(())
    }

    /// `StrRev(str)` — reverse `str` in place with two pointers converging from
    /// the ends, swapping bytes until they meet; return `str`. No call inside the
    /// loop, so the cursors stay in registers.
    fn gen_str_rev(&mut self, arg: &Expr) -> Result<(), CodegenError> {
        self.gen_expr(arg)?; // RES = str
        let str_off = self.alloc(8, 8);
        self.asm.sub_imm(T2, FP, str_off);
        self.gen_store(RES, T2, &Type::I64); // save str (base + result)

        // q = str + strlen(str) - 1 ; p stays in a register from the base.
        self.load_local(0, str_off, &Type::I64);
        self.asm.bl_extern("_strlen"); // x0 = len
        self.load_local(RES, str_off, &Type::I64); // p = base
        self.asm.add(T2, RES, 0); // T2 = base + len
        self.asm.sub_imm(T2, T2, 1); // q = base + len - 1

        let l_loop = self.asm.new_label();
        let l_end = self.asm.new_label();
        self.asm.place(l_loop);
        self.asm.cmp_reg(RES, T2); // p - q
        self.asm.b_cond(COND_HS, l_end); // p >= q (unsigned) -> done
        self.asm.load_mem(SCRATCH, RES, 1, false); // SCRATCH = *p
        self.asm.load_mem(0, T2, 1, false); // x0 = *q
        self.asm.store_mem(SCRATCH, T2, 1); // *q = old *p
        self.asm.store_mem(0, RES, 1); // *p = old *q
        self.asm.add_imm(RES, RES, 1); // p++
        self.asm.sub_imm(T2, T2, 1); // q--
        self.asm.b(l_loop);
        self.asm.place(l_end);
        self.load_local(RES, str_off, &Type::I64); // return str
        Ok(())
    }
}

/// Where an aggregate being initialised lives: a local frame slot (`x29 - off`)
/// or a global symbol.
enum Place {
    Local(u32),
    Global(u32),
}

/// The callee of an `emit_call`: a local function (resolved by label), an
/// undefined external libc symbol (resolved by the linker), or an indirect call
/// through a function-pointer value (the callee expression).
#[derive(Clone, Copy)]
enum CallTarget<'a> {
    Label(usize),
    Extern(&'static str),
    Indirect(&'a Expr),
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

/// Accumulators for the per-function register-promotion analysis. Besides the
/// eligibility data (address-taken names, reference counts, declaration
/// count/type), it walks the body assigning each reference a monotonic *program
/// point* (`pt`) to build a live interval `[first, last]` per name, records each
/// structured loop's point range (for liveness extension), and notes whether the
/// function uses `goto`/labels (`unstructured` — then sharing is disabled).
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
    /// Loop-nesting depth at the current scan point. A use inside a loop counts
    /// for more (it runs every iteration), so loop-invariant reads — a loop bound,
    /// say — get promoted even when their *static* count is just one.
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
                    // The declaration *defines* the variable; record it after the
                    // initializer's uses so a `t = expr` over disjoint values can
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
                // The init runs once *before* the loop, so it's outside the range
                // and the depth bump — only cond/step/body repeat each iteration.
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
            // A nested function/class has its own scope; its body never refers to
            // this function's locals.
            StmtKind::Func(_) | StmtKind::Class(_) => {}
        }
    }

    fn scan_expr(&mut self, e: &Expr) {
        match &e.kind {
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

/// Decide which of a function's scalar locals/params live in callee-saved
/// registers, and *which* register each gets. Eligible candidates (a
/// non-address-taken scalar declared exactly once and referenced ≥2 times) are
/// allocated by **linear scan over their live intervals**, so two whose ranges
/// don't overlap share a register — fewer distinct registers means less
/// prologue/epilogue save/restore, and more than a pool's worth of locals can be
/// promoted. Integer/pointer locals draw from x19..x28, F64 locals from d8..d15.
///
/// Soundness: a live interval is `[first reference, last reference]` (references
/// include the defining declaration), conservatively *over*-approximated —
/// extended to cover any structured loop it touches (so loop-carried values stay
/// live across the back-edge), and widened to the whole function when the body
/// uses `goto`/labels (then nothing shares). Two variables get the same register
/// only when their intervals are strictly disjoint. Deterministic throughout.
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

    // Extend each interval to cover any loop it intersects (to a fixpoint, so
    // nested loops widen outward) — loop-carried values are live across the
    // back-edge, beyond their textual last reference.
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

    // Linear scan: process intervals by start point; a register frees when its
    // interval ends strictly before the next one begins. Ties favor the
    // most-referenced (then name) so hot variables win a scarce register.
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
        // Pool full: spill the coldest active interval of this register file if it
        // is strictly colder than `c`, handing its register to the hotter variable
        // (a whole-range swap — the evicted one falls back to a frame slot). This
        // is the standard linear-scan spill, ranked by the same loop-weighted
        // hotness as promotion rather than by interval end.
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
            // else: `c` is no hotter than any active interval — leave it in a slot.
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

/// Encode `+v` as an `add`/`sub` 12-bit immediate. Returns `(is_sub, imm)` where
/// `imm` is in `0..4096`, or `None` if `v` doesn't fit (caller uses a register).
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

fn translate_format(fmt: &str) -> Result<String, CodegenError> {
    let mut out = String::new();
    let mut chars = fmt.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        // Parse the full spec (flags/width/precision/length) and reconstruct it
        // with the `ll` length on integer conversions, so libc reads the 64-bit
        // argument and honors the same flags the interpreter does.
        let spec = crate::fmt::parse(&mut chars);
        out.push_str(&crate::fmt::to_c_format(&spec));
    }
    Ok(out)
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
