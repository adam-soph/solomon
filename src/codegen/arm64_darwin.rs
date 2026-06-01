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
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use super::{Codegen, CodegenError};
use crate::ast::*;
use crate::layout::Layouts;
use crate::token::{Pos, Span};

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

impl Arm64Darwin {
    pub fn new(out_path: impl Into<PathBuf>) -> Self {
        Arm64Darwin {
            out_path: out_path.into(),
        }
    }

    fn compile(&self, program: &Program) -> Result<Vec<u8>, CodegenError> {
        let (layouts, _) = crate::layout::compute(program);
        let mut cg = Cg::new(layouts);

        let main_label = cg.asm.new_label();
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
        // External (libc) symbols, in first-reference order. They are placed in
        // the symbol table after the defined symbols and common globals, so each
        // gets index `ndefined + commons.len() + position`.
        let mut externs: Vec<&'static str> = Vec::new();
        for (_, sym, _) in &image.relocs {
            if let SymRef::Extern(name) = sym {
                if !externs.contains(name) {
                    externs.push(name);
                }
            }
        }
        let extern_base = ndefined + commons.len() as u32;
        let relocs: Vec<(u32, u32, u32, bool)> = image
            .relocs
            .iter()
            .map(|(addr, sym, kind)| {
                let s = match sym {
                    SymRef::Extern(name) => {
                        extern_base + externs.iter().position(|e| e == name).unwrap() as u32
                    }
                    SymRef::Sym(i) => *i,
                };
                let (ty, pcrel) = match kind {
                    RelKind::Branch26 => (RELOC_BRANCH26, true),
                    RelKind::Page21 => (RELOC_PAGE21, true),
                    RelKind::PageOff12 => (RELOC_PAGEOFF12, false),
                };
                (*addr, s, ty, pcrel)
            })
            .collect();
        Ok(write_macho_object(
            &image.text,
            &defined,
            &commons,
            &externs,
            &relocs,
        ))
    }

    fn link(&self, obj: &Path) -> Result<(), CodegenError> {
        let status = Command::new("cc")
            .arg(obj)
            .arg("-o")
            .arg(&self.out_path)
            .status()
            .map_err(|e| CodegenError::new(format!("failed to invoke linker `cc`: {e}"), None))?;
        if !status.success() {
            return Err(CodegenError::new(
                format!("linker `cc` failed with status {status}"),
                None,
            ));
        }
        Ok(())
    }
}

impl Codegen for Arm64Darwin {
    fn name(&self) -> &'static str {
        "aarch64-apple-darwin"
    }

    fn run(&mut self, program: &Program) -> Result<(), CodegenError> {
        let macho = self.compile(program)?;
        static OBJ_SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = OBJ_SEQ.fetch_add(1, Ordering::Relaxed);
        let obj = std::env::temp_dir().join(format!("solomon-{}-{seq}.o", std::process::id()));
        fs::write(&obj, &macho)
            .map_err(|e| CodegenError::new(format!("cannot write object file: {e}"), None))?;
        let result = self.link(&obj);
        let _ = fs::remove_file(&obj);
        result
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
}

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
            self.asm.adrp_global(RES, sym);
            self.asm.add_global(RES, RES, sym);
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
            self.asm.adrp_global(T2, sym);
            self.asm.add_global(T2, T2, sym);
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
            self.asm.adrp_global(T2, sym);
            self.asm.add_global(T2, T2, sym);
            self.asm.fmov_to_gpr(RES, FRES);
            self.asm.store_mem(RES, T2, 8);
        } else {
            self.gen_int_expr(init, &ty)?; // value -> RES
            self.asm.adrp_global(T2, sym);
            self.asm.add_global(T2, T2, sym);
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
                self.asm.adrp_global(dst, *sym);
                self.asm.add_global(dst, dst, *sym);
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
            self.asm.adrp_global(T2, sym);
            self.asm.add_global(T2, T2, sym); // T2 = &state
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
        // `StrToUpper`/`StrToLower` — ASCII-case a string in place via an inline
        // loop calling libc `toupper`/`tolower`; return the string.
        if name == "StrToUpper" || name == "StrToLower" {
            return self.gen_str_case(&args[0], name == "StrToUpper");
        }
        if name == "StrRev" {
            return self.gen_str_rev(&args[0]);
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
            let ret = crate::builtins::all()
                .into_iter()
                .find(|b| b.name == name)
                .map(|b| b.ret)
                .unwrap_or(Type::I64);
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
            if name == "Print" {
                return self.gen_print_call(args, pos);
            }
            if name == "StrPrint" {
                return self.gen_formatted_write(args, pos, false);
            }
            if name == "CatPrint" {
                return self.gen_formatted_write(args, pos, true);
            }
            if name == "MStrPrint" {
                return self.gen_mstrprint(args, pos);
            }
            if name == "I64ToStr" {
                return self.gen_tostr(&args[0], &args[1], "%d", false, pos);
            }
            if name == "F64ToStr" {
                return self.gen_tostr(&args[0], &args[1], "%g", true, pos);
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

    fn gen_print(&mut self, fmt: &str, args: &[Expr]) -> Result<(), CodegenError> {
        let c_fmt = translate_format(fmt)?;
        let fmt_idx = self.asm.intern_string(&c_fmt);
        let k = args.len() as u32;
        let varsize = align16(k * 8);
        if varsize > 0 {
            self.asm.sub_sp_imm(varsize);
        }
        for (i, arg) in args.iter().enumerate() {
            // Apple arm64 passes *all* variadic arguments on the stack, so each
            // one (int or double) lands in its 8-byte slot the same way.
            if is_f64(&self.expr_ty(arg)) {
                self.gen_fexpr(arg)?;
                self.asm.fmov_to_gpr(RES, FRES);
            } else {
                self.gen_expr(arg)?;
            }
            self.asm.str_sp(RES, i as u32 * 8);
        }
        self.asm.adr(0, fmt_idx);
        self.asm.bl_printf();
        if varsize > 0 {
            self.asm.add_sp_imm(varsize);
        }
        Ok(())
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
        self.load_local(0, target_off, &Type::I64); // x0 = target
        self.asm.adr(1, fmt_idx); // x1 = format
        self.asm.bl_extern("_sprintf");
        if varsize > 0 {
            self.asm.add_sp_imm(varsize);
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
        self.asm.load_imm(0, 0); // x0 = NULL
        self.asm.load_imm(1, 0); // x1 = 0
        self.asm.adr(2, fmt_idx); // x2 = format
        self.asm.bl_extern("_snprintf");
        self.asm.ubfm(0, 0, 0, 31); // x0 = (u32) len
        self.asm.add_imm(0, 0, 1); // + 1 for the NUL
        self.asm.bl_extern("_malloc"); // x0 = buf
        self.asm.sub_imm(T2, FP, buf_off);
        self.gen_store(0, T2, &Type::I64); // save buf
        // sprintf(buf, fmt, ...) reads the same variadic args still on the stack.
        self.load_local(0, buf_off, &Type::I64); // x0 = buf
        self.asm.adr(1, fmt_idx); // x1 = format
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
        self.asm.bl_extern("_sprintf");
        self.asm.add_sp_imm(16);
        self.load_local(RES, buf_off, &Type::I64); // return buf
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

// ---- AArch64 instruction encoder with backpatching ----

#[derive(Clone, Copy)]
enum Fixup {
    B26,
    B19,
    /// ADR rd, label — a PC-relative address of an in-`__text` label (a function
    /// entry), for taking a function's address (`&Func`).
    Adr,
    /// A 32-bit jump-table data word: the byte distance from a base label (the
    /// table start, carried here) to the target label (the tuple's label id).
    /// `BR (table + word)` then lands on the target — a section-internal offset
    /// computed at emit time, so it needs no Mach-O relocation.
    TableRel(usize),
}

/// The symbol a relocation refers to. `Extern(name)` is an undefined external
/// symbol (a libc function such as `_printf`/`_strlen`); its final symbol index
/// is resolved late, after the symbol-table layout is known. `Sym(i)` is an
/// already-final symbol index (a global).
#[derive(Clone, Copy)]
enum SymRef {
    Extern(&'static str),
    Sym(u32),
}

#[derive(Clone, Copy)]
enum RelKind {
    Branch26,
    Page21,
    PageOff12,
}

struct CodeImage {
    text: Vec<u8>,
    /// `(byte offset in __text, symbol, kind)` relocations the linker resolves.
    relocs: Vec<(u32, SymRef, RelKind)>,
}

struct Asm {
    words: Vec<u32>,
    label_pos: Vec<Option<usize>>,
    fixups: Vec<(usize, usize, Fixup)>,
    strings: Vec<Vec<u8>>,
    string_dedup: HashMap<Vec<u8>, usize>,
    adr_fixups: Vec<(usize, usize)>,
    relocs: Vec<(usize, SymRef, RelKind)>,
    // Liveness tags, parallel to `words` (one entry per emitted instruction word).
    inst_def: Vec<i8>,    // GP register written (0..30), or -1 for none / multi-def
    inst_use: Vec<u32>,   // bitmask of GP registers read (over-approximated)
    inst_branch: Vec<u8>, // B_NORMAL / B_CALL / B_RET / B_BRANCH
}

impl Asm {
    fn new() -> Self {
        Asm {
            words: Vec::new(),
            label_pos: Vec::new(),
            fixups: Vec::new(),
            strings: Vec::new(),
            string_dedup: HashMap::new(),
            adr_fixups: Vec::new(),
            relocs: Vec::new(),
            inst_def: Vec::new(),
            inst_use: Vec::new(),
            inst_branch: Vec::new(),
        }
    }

    /// Emit a word with conservative tags: defines nothing the peephole can use,
    /// reads everything, and acts as a barrier. Emitters with known register
    /// behavior call the tagged variants below so the liveness scan can see
    /// through them; anything left on this path is simply never optimized across.
    fn emit(&mut self, word: u32) {
        self.words.push(word);
        self.inst_def.push(-1);
        self.inst_use.push(GP_ALL);
        self.inst_branch.push(B_BRANCH);
    }
    /// Emit a word with explicit liveness tags.
    fn emit_du(&mut self, word: u32, def: i32, uses: u32, branch: u8) {
        self.words.push(word);
        self.inst_def.push(if (0..31).contains(&def) {
            def as i8
        } else {
            -1
        });
        self.inst_use.push(uses & GP_ALL);
        self.inst_branch.push(branch);
    }
    /// rd = f(rn, rm)
    fn e_rrr(&mut self, word: u32, rd: u32, rn: u32, rm: u32) {
        self.emit_du(word, rd as i32, gpb(rn) | gpb(rm), B_NORMAL);
    }
    /// rd = f(rn)
    fn e_rr(&mut self, word: u32, rd: u32, rn: u32) {
        self.emit_du(word, rd as i32, gpb(rn), B_NORMAL);
    }
    /// rd = constant / from a non-GP source (write-only as far as GP regs go)
    fn e_wr(&mut self, word: u32, rd: u32) {
        self.emit_du(word, rd as i32, 0, B_NORMAL);
    }
    /// no GP destination; reads `uses` (stores, compares)
    fn e_use(&mut self, word: u32, uses: u32) {
        self.emit_du(word, -1, uses, B_NORMAL);
    }
    /// touches no GP register the peephole tracks (FP-only / SP-only ops)
    fn e_nogp(&mut self, word: u32) {
        self.emit_du(word, -1, 0, B_NORMAL);
    }
    fn new_label(&mut self) -> usize {
        self.label_pos.push(None);
        self.label_pos.len() - 1
    }
    fn place(&mut self, id: usize) {
        self.label_pos[id] = Some(self.words.len());
    }
    fn label_byte(&self, id: usize) -> Result<u64, CodegenError> {
        self.label_pos[id]
            .map(|w| (w * 4) as u64)
            .ok_or_else(|| CodegenError::new("internal: unplaced function label", None))
    }

    fn intern_string(&mut self, s: &str) -> usize {
        let mut bytes = s.as_bytes().to_vec();
        bytes.push(0);
        if let Some(&i) = self.string_dedup.get(&bytes) {
            return i;
        }
        let i = self.strings.len();
        self.string_dedup.insert(bytes.clone(), i);
        self.strings.push(bytes);
        i
    }

    /// Whether GP register `reg` (a caller-saved scratch — x9/x10) is dead
    /// immediately after `words[m]`: overwritten, clobbered by a call, or unused
    /// through the function's return, before any read. Conservative — any plain
    /// branch ends the scan as "live" since the scan only follows fall-through.
    fn dead_after(&self, m: usize, reg: u32) -> bool {
        let bit = 1u32 << reg;
        let mut j = m + 1;
        while j < self.words.len() {
            match self.inst_branch[j] {
                B_NORMAL => {
                    if self.inst_use[j] & bit != 0 {
                        return false; // read -> live
                    }
                    if self.inst_def[j] == reg as i8 {
                        return true; // overwritten before any read -> dead
                    }
                }
                B_CALL => {
                    if self.inst_use[j] & bit != 0 {
                        return false; // the call is *through* this register
                    }
                    return true; // x9/x10 are caller-saved: the call clobbers them
                }
                B_RET => return true, // x9/x10 are not live-out of a function
                _ => return false,    // any other branch: conservative barrier
            }
            j += 1;
        }
        true
    }

    /// Liveness-driven dead-`mov` elimination, run once before fixups resolve.
    /// Removes `mov Xd, Xs` moves that can't change observable behavior, then
    /// remaps every stored word-index position past the removed words. Restricted
    /// to the pure scratch temporaries x9 (RES) / x10 (T2), which are never live
    /// across a call or return and are never ABI registers. Two cases:
    ///   * removal — Xd is dead after the move, so the copy is pointless;
    ///   * fusion  — the immediately-preceding instruction produced Xs and Xs is
    ///     dead after the move, so that instruction can target Xd directly.
    fn peephole(&mut self) {
        let n = self.words.len();
        if n == 0 {
            return;
        }
        // A move a label points at is a branch target — never touch it.
        let mut label_here = vec![false; n + 1];
        for p in self.label_pos.iter().flatten() {
            if *p < label_here.len() {
                label_here[*p] = true;
            }
        }
        // Positions carrying a fixup/reloc. A `mov` never does, but a fused-into
        // predecessor might; skip those to keep the rewrite reasoning simple.
        let mut protected = vec![false; n];
        for (at, _, _) in &self.fixups {
            protected[*at] = true;
        }
        for (at, _) in &self.adr_fixups {
            protected[*at] = true;
        }
        for (at, _, _) in &self.relocs {
            protected[*at] = true;
        }

        let mut remove = vec![false; n];
        for m in 0..n {
            let w = self.words[m];
            if (w & 0xFFE0_FFE0) != 0xAA00_03E0 {
                continue; // not `mov Xd, Xs` (ORR Xd, XZR, Xs, no shift)
            }
            if label_here[m] {
                continue;
            }
            let xd = w & 0x1F;
            let xs = (w >> 16) & 0x1F;
            if xd == xs {
                continue;
            }
            // Removal: a copy into a scratch register that is never read again.
            if (xd == RES || xd == T2) && self.dead_after(m, xd) {
                remove[m] = true;
                continue;
            }
            // Fusion: let the producer of Xs write Xd directly and drop the copy.
            if (xs == RES || xs == T2)
                && m >= 1
                && !remove[m - 1]
                && !protected[m - 1]
                && self.inst_def[m - 1] == xs as i8
                && (self.words[m - 1] & 0xFF80_0000) != 0xF280_0000 // movk reads its own Rd
                && self.dead_after(m, xs)
            {
                self.words[m - 1] = (self.words[m - 1] & !0x1F) | xd;
                self.inst_def[m - 1] = xd as i8;
                remove[m] = true;
            }
        }
        if !remove.iter().any(|&r| r) {
            return;
        }

        // Compact the word stream and remap every word-index position. Label ids
        // and `TableRel`'s base are label indices (resolved through label_pos), so
        // only label_pos entries and the `.0` of fixups/adr_fixups/relocs move.
        let mut shift = vec![0usize; n + 1];
        for i in 0..n {
            shift[i + 1] = shift[i] + remove[i] as usize;
        }
        let remap = |p: usize| p - shift[p];

        let keep =
            |v: &[u32]| -> Vec<u32> { (0..n).filter(|&i| !remove[i]).map(|i| v[i]).collect() };
        self.words = keep(&self.words);
        self.inst_use = keep(&self.inst_use);
        self.inst_def = (0..n)
            .filter(|&i| !remove[i])
            .map(|i| self.inst_def[i])
            .collect();
        self.inst_branch = (0..n)
            .filter(|&i| !remove[i])
            .map(|i| self.inst_branch[i])
            .collect();

        for lp in self.label_pos.iter_mut() {
            if let Some(p) = lp {
                *p = remap(*p);
            }
        }
        for f in self.fixups.iter_mut() {
            f.0 = remap(f.0);
        }
        for a in self.adr_fixups.iter_mut() {
            a.0 = remap(a.0);
        }
        for r in self.relocs.iter_mut() {
            r.0 = remap(r.0);
        }
    }

    fn finish(mut self) -> Result<CodeImage, CodegenError> {
        self.peephole();
        for (at, id, kind) in &self.fixups {
            let target = self.label_pos[*id]
                .ok_or_else(|| CodegenError::new("internal: unplaced code label", None))?;
            let off = target as i64 - *at as i64;
            match kind {
                Fixup::B26 => self.words[*at] |= (off as u32) & 0x03FF_FFFF,
                Fixup::B19 => self.words[*at] |= ((off as u32) & 0x7_FFFF) << 5,
                Fixup::Adr => {
                    let imm = off * 4; // ADR immediate is in bytes
                    if !(-(1 << 20)..(1 << 20)).contains(&imm) {
                        return Err(CodegenError::new("function too far for ADR (>1MB)", None));
                    }
                    let immlo = (imm as u32) & 0x3;
                    let immhi = ((imm as u32) >> 2) & 0x7_FFFF;
                    self.words[*at] |= (immlo << 29) | (immhi << 5);
                }
                Fixup::TableRel(base) => {
                    let base_pos = self.label_pos[*base]
                        .ok_or_else(|| CodegenError::new("internal: unplaced table label", None))?;
                    // Byte distance table_base -> target (positions are word indices).
                    let off_bytes = (target as i64 - base_pos as i64) * 4;
                    self.words[*at] = off_bytes as u32; // a full data word, not a field
                }
            }
        }
        let code_bytes = self.words.len() * 4;
        let mut str_offsets = Vec::with_capacity(self.strings.len());
        let mut cursor = code_bytes;
        for s in &self.strings {
            str_offsets.push(cursor);
            cursor += s.len();
        }
        for (at, sidx) in &self.adr_fixups {
            let imm = str_offsets[*sidx] as i64 - (*at * 4) as i64;
            if !(-(1 << 20)..(1 << 20)).contains(&imm) {
                return Err(CodegenError::new("string too far for ADR (>1MB)", None));
            }
            let immlo = (imm as u32) & 0x3;
            let immhi = ((imm as u32) >> 2) & 0x7_FFFF;
            self.words[*at] |= (immlo << 29) | (immhi << 5);
        }
        let mut text = Vec::with_capacity(cursor);
        for w in &self.words {
            text.extend_from_slice(&w.to_le_bytes());
        }
        for s in &self.strings {
            text.extend_from_slice(s);
        }
        let relocs = self
            .relocs
            .iter()
            .map(|(w, sym, kind)| ((*w * 4) as u32, *sym, *kind))
            .collect();
        Ok(CodeImage { text, relocs })
    }

    // data processing
    fn load_imm(&mut self, rd: u32, value: i64) {
        let v = value as u64;
        self.e_wr(0xD280_0000 | ((v as u32 & 0xFFFF) << 5) | rd, rd); // movz
        for hw in 1..4u32 {
            let half = ((v >> (16 * hw)) & 0xFFFF) as u32;
            if half != 0 {
                // movk reads-modifies rd (its source register *is* its destination),
                // so it must not be a fusion target.
                self.e_rr(0xF280_0000 | (hw << 21) | (half << 5) | rd, rd, rd);
            }
        }
    }
    fn add(&mut self, rd: u32, rn: u32, rm: u32) {
        self.e_rrr(0x8B00_0000 | (rm << 16) | (rn << 5) | rd, rd, rn, rm);
    }
    fn sub(&mut self, rd: u32, rn: u32, rm: u32) {
        self.e_rrr(0xCB00_0000 | (rm << 16) | (rn << 5) | rd, rd, rn, rm);
    }
    /// SXTW rd, wn — sign-extend a 32-bit word to 64 bits (an `int` libc return
    /// leaves the upper 32 bits of the x-register unspecified).
    fn sxtw(&mut self, rd: u32, rn: u32) {
        self.e_rr(0x9340_7C00 | (rn << 5) | rd, rd, rn);
    }
    fn mul(&mut self, rd: u32, rn: u32, rm: u32) {
        self.e_rrr(
            0x9B00_0000 | (rm << 16) | (XZR << 10) | (rn << 5) | rd,
            rd,
            rn,
            rm,
        );
    }
    fn msub(&mut self, rd: u32, rn: u32, rm: u32, ra: u32) {
        let w = 0x9B00_8000 | (rm << 16) | (ra << 10) | (rn << 5) | rd;
        self.emit_du(w, rd as i32, gpb(rn) | gpb(rm) | gpb(ra), B_NORMAL);
    }
    fn madd(&mut self, rd: u32, rn: u32, rm: u32, ra: u32) {
        let w = 0x9B00_0000 | (rm << 16) | (ra << 10) | (rn << 5) | rd;
        self.emit_du(w, rd as i32, gpb(rn) | gpb(rm) | gpb(ra), B_NORMAL);
    }
    fn sdiv(&mut self, rd: u32, rn: u32, rm: u32) {
        self.e_rrr(0x9AC0_0C00 | (rm << 16) | (rn << 5) | rd, rd, rn, rm);
    }
    fn udiv(&mut self, rd: u32, rn: u32, rm: u32) {
        self.e_rrr(0x9AC0_0800 | (rm << 16) | (rn << 5) | rd, rd, rn, rm);
    }
    fn and(&mut self, rd: u32, rn: u32, rm: u32) {
        self.e_rrr(0x8A00_0000 | (rm << 16) | (rn << 5) | rd, rd, rn, rm);
    }
    fn orr(&mut self, rd: u32, rn: u32, rm: u32) {
        self.e_rrr(0xAA00_0000 | (rm << 16) | (rn << 5) | rd, rd, rn, rm);
    }
    fn eor(&mut self, rd: u32, rn: u32, rm: u32) {
        self.e_rrr(0xCA00_0000 | (rm << 16) | (rn << 5) | rd, rd, rn, rm);
    }
    fn lslv(&mut self, rd: u32, rn: u32, rm: u32) {
        self.e_rrr(0x9AC0_2000 | (rm << 16) | (rn << 5) | rd, rd, rn, rm);
    }
    fn lsrv(&mut self, rd: u32, rn: u32, rm: u32) {
        self.e_rrr(0x9AC0_2400 | (rm << 16) | (rn << 5) | rd, rd, rn, rm);
    }
    /// ASRV Xd, Xn, Xm — arithmetic (sign-preserving) shift right by a register.
    fn asrv(&mut self, rd: u32, rn: u32, rm: u32) {
        self.e_rrr(0x9AC0_2800 | (rm << 16) | (rn << 5) | rd, rd, rn, rm);
    }
    fn neg(&mut self, rd: u32, rm: u32) {
        self.sub(rd, XZR, rm);
    }
    fn mvn(&mut self, rd: u32, rm: u32) {
        self.e_rr(0xAA20_0000 | (rm << 16) | (XZR << 5) | rd, rd, rm);
    }
    fn mov_reg(&mut self, rd: u32, rm: u32) {
        if rd != rm {
            self.orr(rd, XZR, rm); // a move to itself is a no-op
        }
    }
    /// SBFM Xd, Xn, #immr, #imms (used for sign-extend casts).
    fn sbfm(&mut self, rd: u32, rn: u32, immr: u32, imms: u32) {
        self.e_rr(
            0x9340_0000 | (immr << 16) | (imms << 10) | (rn << 5) | rd,
            rd,
            rn,
        );
    }
    /// UBFM Xd, Xn, #immr, #imms (used for zero-extend casts).
    fn ubfm(&mut self, rd: u32, rn: u32, immr: u32, imms: u32) {
        self.e_rr(
            0xD340_0000 | (immr << 16) | (imms << 10) | (rn << 5) | rd,
            rd,
            rn,
        );
    }

    // scalar double-precision floating point (F64 lives in v-registers)
    /// FMOV Xd, Dn — move the raw 64 bits of a double into a GPR.
    fn fmov_to_gpr(&mut self, xd: u32, dn: u32) {
        self.e_wr(0x9E66_0000 | (dn << 5) | xd, xd);
    }
    /// FMOV Dd, Xn — move raw 64 bits from a GPR into a double register.
    fn fmov_from_gpr(&mut self, dd: u32, xn: u32) {
        self.e_use(0x9E67_0000 | (xn << 5) | dd, gpb(xn));
    }
    /// FMOV Dd, Dn — copy one double register to another.
    fn fmov_reg(&mut self, dd: u32, dn: u32) {
        self.e_nogp(0x1E60_4000 | (dn << 5) | dd);
    }
    fn fadd(&mut self, dd: u32, dn: u32, dm: u32) {
        self.e_nogp(0x1E60_2800 | (dm << 16) | (dn << 5) | dd);
    }
    fn fsub(&mut self, dd: u32, dn: u32, dm: u32) {
        self.e_nogp(0x1E60_3800 | (dm << 16) | (dn << 5) | dd);
    }
    fn fmul(&mut self, dd: u32, dn: u32, dm: u32) {
        self.e_nogp(0x1E60_0800 | (dm << 16) | (dn << 5) | dd);
    }
    fn fdiv(&mut self, dd: u32, dn: u32, dm: u32) {
        self.e_nogp(0x1E60_1800 | (dm << 16) | (dn << 5) | dd);
    }
    fn fneg(&mut self, dd: u32, dn: u32) {
        self.e_nogp(0x1E61_4000 | (dn << 5) | dd);
    }
    /// FCMP Dn, Dm — set NZCV for an ordered comparison.
    fn fcmp(&mut self, dn: u32, dm: u32) {
        self.e_nogp(0x1E60_2000 | (dm << 16) | (dn << 5));
    }
    /// FCMP Dn, #0.0.
    fn fcmp_zero(&mut self, dn: u32) {
        self.e_nogp(0x1E60_2008 | (dn << 5));
    }
    /// SCVTF Dd, Xn — signed 64-bit integer to double.
    fn scvtf(&mut self, dd: u32, xn: u32) {
        self.e_use(0x9E62_0000 | (xn << 5) | dd, gpb(xn));
    }
    /// FCVTZS Xd, Dn — double to signed 64-bit integer (round toward zero).
    fn fcvtzs(&mut self, xd: u32, dn: u32) {
        self.e_wr(0x9E78_0000 | (dn << 5) | xd, xd);
    }
    /// FCVTZU Xd, Dn — convert a double to a 64-bit *unsigned* integer (toward
    /// zero, saturating). Used when the destination integer type is unsigned.
    fn fcvtzu(&mut self, xd: u32, dn: u32) {
        self.e_wr(0x9E79_0000 | (dn << 5) | xd, xd);
    }

    fn add_imm(&mut self, rd: u32, rn: u32, imm: u32) {
        self.e_rr(0x9100_0000 | ((imm & 0xFFF) << 10) | (rn << 5) | rd, rd, rn);
    }
    fn sub_imm(&mut self, rd: u32, rn: u32, imm: u32) {
        self.e_rr(0xD100_0000 | ((imm & 0xFFF) << 10) | (rn << 5) | rd, rd, rn);
    }
    fn add_sp_imm(&mut self, imm: u32) {
        if imm != 0 {
            self.add_imm(SP, SP, imm); // a 0-byte stack adjust is a no-op
        }
    }
    fn sub_sp_imm(&mut self, imm: u32) {
        if imm != 0 {
            self.sub_imm(SP, SP, imm);
        }
    }

    // frame
    fn stp_pre_fp_lr(&mut self) {
        // stp x29, x30, [sp, #-16]!
        let imm7 = (-2i32 as u32) & 0x7F;
        let w = 0xA980_0000 | (imm7 << 15) | (LR << 10) | (SP << 5) | FP;
        self.e_use(w, gpb(FP) | gpb(LR));
    }
    fn ldp_post_fp_lr(&mut self) {
        // ldp x29, x30, [sp], #16 — writes x29/x30 (neither is a peephole
        // candidate, so it reads as transparent to the x9/x10 liveness scan).
        self.e_nogp(0xA8C0_0000 | (2 << 15) | (LR << 10) | (SP << 5) | FP);
    }
    fn mov_fp_sp(&mut self) {
        self.add_imm(FP, SP, 0);
    }
    fn mov_sp_fp(&mut self) {
        self.add_imm(SP, FP, 0);
    }
    fn emit_sub_sp_placeholder(&mut self) -> usize {
        let idx = self.words.len();
        self.e_nogp(0xD100_0000 | (SP << 5) | SP); // sub sp, sp, #0
        idx
    }
    fn patch_sub_sp(&mut self, idx: usize, imm: u32) {
        self.words[idx] |= (imm & 0xFFF) << 10;
    }

    // width-aware memory (offset 0 from `addr`)
    fn load_mem(&mut self, dst: u32, addr: u32, size: u32, signed: bool) {
        self.load_mem_off(dst, addr, 0, size, signed);
    }
    fn store_mem(&mut self, val: u32, addr: u32, size: u32) {
        self.store_mem_off(val, addr, 0, size);
    }
    /// Width-aware load from `[base, #byte_off]` (the unsigned-offset form, so
    /// `byte_off` must be a multiple of `size`).
    fn load_mem_off(&mut self, dst: u32, base: u32, byte_off: u32, size: u32, signed: bool) {
        let op = match (size, signed) {
            (8, _) => 0xF940_0000,
            (4, true) => 0xB980_0000,
            (4, false) => 0xB940_0000,
            (2, true) => 0x7980_0000,
            (2, false) => 0x7940_0000,
            (1, true) => 0x3980_0000,
            (1, false) => 0x3940_0000,
            _ => 0xF940_0000,
        };
        let imm12 = (byte_off / size) & 0xFFF;
        self.e_rr(op | (imm12 << 10) | (base << 5) | dst, dst, base);
    }
    fn store_mem_off(&mut self, val: u32, base: u32, byte_off: u32, size: u32) {
        let op = match size {
            8 => 0xF900_0000,
            4 => 0xB900_0000,
            2 => 0x7900_0000,
            1 => 0x3900_0000,
            _ => 0xF900_0000,
        };
        let imm12 = (byte_off / size) & 0xFFF;
        self.e_use(op | (imm12 << 10) | (base << 5) | val, gpb(val) | gpb(base));
    }
    /// STR rt, [sp, #off].
    fn str_sp(&mut self, rt: u32, off: u32) {
        self.e_use(0xF900_0000 | ((off / 8) << 10) | (SP << 5) | rt, gpb(rt));
    }
    /// STR reg, [sp, #-16]! (push).
    fn push(&mut self, reg: u32) {
        let imm9 = (-16i32 as u32) & 0x1FF;
        self.e_use(0xF800_0C00 | (imm9 << 12) | (SP << 5) | reg, gpb(reg));
    }
    /// LDR reg, [sp], #16 (pop).
    fn pop(&mut self, reg: u32) {
        let imm9 = 16u32 & 0x1FF;
        self.e_wr(0xF840_0400 | (imm9 << 12) | (SP << 5) | reg, reg);
    }
    /// STUR Xt, [Xn, #simm9] — store at an unscaled signed byte offset (used to
    /// spill a callee-saved register near x29 in one instruction).
    fn stur(&mut self, rt: u32, base: u32, simm9: i32) {
        let imm = (simm9 as u32) & 0x1FF;
        self.e_use(
            0xF800_0000 | (imm << 12) | (base << 5) | rt,
            gpb(rt) | gpb(base),
        );
    }
    /// LDUR Xt, [Xn, #simm9] — load at an unscaled signed byte offset.
    fn ldur(&mut self, rt: u32, base: u32, simm9: i32) {
        let imm = (simm9 as u32) & 0x1FF;
        self.e_rr(0xF840_0000 | (imm << 12) | (base << 5) | rt, rt, base);
    }
    /// STUR Dt, [Xn, #simm9] — spill a callee-saved double register near x29.
    fn fstur(&mut self, dt: u32, base: u32, simm9: i32) {
        let imm = (simm9 as u32) & 0x1FF;
        self.e_use(0xFC00_0000 | (imm << 12) | (base << 5) | dt, gpb(base));
    }
    /// LDUR Dt, [Xn, #simm9] — reload a callee-saved double register.
    fn fldur(&mut self, dt: u32, base: u32, simm9: i32) {
        let imm = (simm9 as u32) & 0x1FF;
        self.e_use(0xFC40_0000 | (imm << 12) | (base << 5) | dt, gpb(base));
    }

    fn cmp_reg(&mut self, rn: u32, rm: u32) {
        self.e_use(
            0xEB00_0000 | (rm << 16) | (rn << 5) | XZR,
            gpb(rn) | gpb(rm),
        );
    }
    fn cmp_imm0(&mut self, rn: u32) {
        self.e_use(0xF100_0000 | (rn << 5) | XZR, gpb(rn));
    }
    /// CMP Xn, #imm12 — SUBS XZR, Xn, #imm (sets flags, discards the result).
    fn cmp_imm(&mut self, rn: u32, imm: u32) {
        self.e_use(
            0xF100_0000 | ((imm & 0xFFF) << 10) | (rn << 5) | XZR,
            gpb(rn),
        );
    }
    /// LSL/LSR/ASR Xd, Xn, #shift — shift by an immediate (aliases of UBFM/SBFM).
    fn lsl_imm(&mut self, rd: u32, rn: u32, sh: u32) {
        self.ubfm(rd, rn, (64 - sh) & 63, 63 - sh);
    }
    fn lsr_imm(&mut self, rd: u32, rn: u32, sh: u32) {
        self.ubfm(rd, rn, sh, 63);
    }
    fn asr_imm(&mut self, rd: u32, rn: u32, sh: u32) {
        self.sbfm(rd, rn, sh, 63);
    }
    /// AND Xd, Xn, #(2^k - 1) — mask to the low `k` bits (1..=63). `2^k-1` is a
    /// run of `k` low ones, which encodes as the logical immediate N=1, immr=0,
    /// imms=k-1 (`x % 2^k` for unsigned `x`).
    fn and_imm_lowbits(&mut self, rd: u32, rn: u32, k: u32) {
        self.e_rr(0x9240_0000 | ((k - 1) << 10) | (rn << 5) | rd, rd, rn);
    }
    fn cset(&mut self, rd: u32, cond: u32) {
        let inv = cond ^ 1;
        self.e_wr(
            0x9A80_0400 | (XZR << 16) | (inv << 12) | (XZR << 5) | rd,
            rd,
        );
    }
    fn ret(&mut self) {
        self.emit_du(0xD65F_03C0, -1, 0, B_RET);
    }

    fn b(&mut self, label: usize) {
        self.fixups.push((self.words.len(), label, Fixup::B26));
        self.emit(0x1400_0000); // barrier (default tags)
    }
    fn bl(&mut self, label: usize) {
        self.fixups.push((self.words.len(), label, Fixup::B26));
        self.emit_du(0x9400_0000, -1, 0, B_CALL);
    }
    /// BLR Xn — call the function whose entry address is in register `rn`.
    fn blr(&mut self, rn: u32) {
        self.emit_du(0xD63F_0000 | (rn << 5), -1, gpb(rn), B_CALL);
    }
    /// BR Xn — unconditional branch to the address in `rn` (no link). Used to
    /// jump into a branch table.
    fn br(&mut self, rn: u32) {
        self.emit(0xD61F_0000 | (rn << 5)); // barrier (default tags)
    }
    /// LDRSW Xt, [Xn, Xm, LSL #2] — load a 32-bit word at `base + index*4`,
    /// sign-extended to 64 bits. Used to read a jump-table offset entry.
    fn ldrsw_reg(&mut self, rt: u32, base: u32, index: u32) {
        let w = 0xB8A0_7800 | (index << 16) | (base << 5) | rt;
        self.emit_du(w, rt as i32, gpb(base) | gpb(index), B_NORMAL);
    }
    /// Emit a 32-bit jump-table data word holding the byte distance from `base`
    /// (the table start) to `target`; resolved in `finish` via `Fixup::TableRel`.
    fn table_word(&mut self, base: usize, target: usize) {
        self.fixups
            .push((self.words.len(), target, Fixup::TableRel(base)));
        self.emit(0);
    }
    /// ADR rd, label — load the PC-relative address of an in-`__text` label (a
    /// function entry) into `rd`.
    fn adr_label(&mut self, rd: u32, label: usize) {
        self.fixups.push((self.words.len(), label, Fixup::Adr));
        self.e_wr(0x1000_0000 | rd, rd);
    }
    fn bl_printf(&mut self) {
        self.bl_extern("_printf");
    }
    /// `bl <extern>` — a call to an undefined external (libc) symbol, resolved by
    /// the linker via a BRANCH26 relocation.
    fn bl_extern(&mut self, sym: &'static str) {
        self.relocs
            .push((self.words.len(), SymRef::Extern(sym), RelKind::Branch26));
        self.emit_du(0x9400_0000, -1, 0, B_CALL);
    }
    fn adr(&mut self, rd: u32, sidx: usize) {
        self.adr_fixups.push((self.words.len(), sidx));
        self.e_wr(0x1000_0000 | rd, rd);
    }
    /// ADRP rd, sym@PAGE (the linker fills the immediate via a PAGE21 reloc).
    fn adrp_global(&mut self, rd: u32, sym: u32) {
        self.relocs
            .push((self.words.len(), SymRef::Sym(sym), RelKind::Page21));
        self.e_wr(0x9000_0000 | rd, rd);
    }
    /// ADD rd, rn, sym@PAGEOFF (filled via a PAGEOFF12 reloc).
    fn add_global(&mut self, rd: u32, rn: u32, sym: u32) {
        self.relocs
            .push((self.words.len(), SymRef::Sym(sym), RelKind::PageOff12));
        self.e_rr(0x9100_0000 | (rn << 5) | rd, rd, rn);
    }
    fn b_cond(&mut self, cond: u32, label: usize) {
        self.fixups.push((self.words.len(), label, Fixup::B19));
        self.emit(0x5400_0000 | cond);
    }
    fn cbz(&mut self, rt: u32, label: usize) {
        self.fixups.push((self.words.len(), label, Fixup::B19));
        self.emit(0xB400_0000 | rt);
    }
    fn cbnz(&mut self, rt: u32, label: usize) {
        self.fixups.push((self.words.len(), label, Fixup::B19));
        self.emit(0xB500_0000 | rt);
    }
}

// ---- Mach-O relocatable object writer (arm64) ----

const RELOC_BRANCH26: u32 = 2;
const RELOC_PAGE21: u32 = 3;
const RELOC_PAGEOFF12: u32 = 4;

/// Build the Mach-O object. Symbols are laid out as: defined (`_main` + funcs,
/// in `__text`), then common globals, then the undefined externals (libc
/// functions) — matching the indices the relocations were built with. Globals
/// are *common* symbols (`n_value` = size), so the linker allocates their
/// storage; no data section is needed.
fn write_macho_object(
    text: &[u8],
    defined: &[(String, u64)],
    commons: &[(String, u64, u32)],
    externs: &[&str],
    relocs: &[(u32, u32, u32, bool)],
) -> Vec<u8> {
    let mut strtab = vec![0u8];
    let strx = |s: &mut Vec<u8>, name: &str| -> u32 {
        let at = s.len() as u32;
        s.extend_from_slice(name.as_bytes());
        s.push(0);
        at
    };
    let defined_strx: Vec<u32> = defined.iter().map(|(n, _)| strx(&mut strtab, n)).collect();
    let common_strx: Vec<u32> = commons
        .iter()
        .map(|(n, _, _)| strx(&mut strtab, n))
        .collect();
    let extern_strx: Vec<u32> = externs.iter().map(|n| strx(&mut strtab, n)).collect();

    let nsyms = defined.len() as u32 + commons.len() as u32 + externs.len() as u32;
    let nundef = commons.len() as u32 + externs.len() as u32;

    const HEADER: usize = 32;
    const SEG_CMD: usize = 72 + 80;
    const SYMTAB_CMD: usize = 24;
    const DYSYMTAB_CMD: usize = 80;
    const BUILD_CMD: usize = 24;
    let sizeofcmds = SEG_CMD + SYMTAB_CMD + DYSYMTAB_CMD + BUILD_CMD;

    let code_off = HEADER + sizeofcmds;
    let reloc_off = align8(code_off + text.len());
    let nreloc = relocs.len();
    let sym_off = align8(reloc_off + nreloc * 8);
    let str_off = sym_off + (nsyms as usize) * 16;

    let mut b = Vec::new();

    put_u32(&mut b, 0xFEED_FACF);
    put_u32(&mut b, 0x0100_000C);
    put_u32(&mut b, 0x0000_0000);
    put_u32(&mut b, 1);
    put_u32(&mut b, 4);
    put_u32(&mut b, sizeofcmds as u32);
    put_u32(&mut b, 0);
    put_u32(&mut b, 0);

    put_u32(&mut b, 0x19);
    put_u32(&mut b, SEG_CMD as u32);
    put_name16(&mut b, "");
    put_u64(&mut b, 0);
    put_u64(&mut b, text.len() as u64);
    put_u64(&mut b, code_off as u64);
    put_u64(&mut b, text.len() as u64);
    put_u32(&mut b, 7);
    put_u32(&mut b, 7);
    put_u32(&mut b, 1);
    put_u32(&mut b, 0);
    put_name16(&mut b, "__text");
    put_name16(&mut b, "__TEXT");
    put_u64(&mut b, 0);
    put_u64(&mut b, text.len() as u64);
    put_u32(&mut b, code_off as u32);
    put_u32(&mut b, 2);
    put_u32(&mut b, reloc_off as u32);
    put_u32(&mut b, nreloc as u32);
    put_u32(&mut b, 0x0000_0400);
    put_u32(&mut b, 0);
    put_u32(&mut b, 0);
    put_u32(&mut b, 0);

    put_u32(&mut b, 0x02);
    put_u32(&mut b, SYMTAB_CMD as u32);
    put_u32(&mut b, sym_off as u32);
    put_u32(&mut b, nsyms);
    put_u32(&mut b, str_off as u32);
    put_u32(&mut b, strtab.len() as u32);

    put_u32(&mut b, 0x0B);
    put_u32(&mut b, DYSYMTAB_CMD as u32);
    put_u32(&mut b, 0); // ilocalsym
    put_u32(&mut b, 0); // nlocalsym
    put_u32(&mut b, 0); // iextdefsym
    put_u32(&mut b, defined.len() as u32); // nextdefsym
    put_u32(&mut b, defined.len() as u32); // iundefsym
    put_u32(&mut b, nundef); // nundefsym
    for _ in 0..12 {
        put_u32(&mut b, 0);
    }

    put_u32(&mut b, 0x32);
    put_u32(&mut b, BUILD_CMD as u32);
    put_u32(&mut b, 1);
    put_u32(&mut b, 0x000B_0000);
    put_u32(&mut b, 0x000B_0000);
    put_u32(&mut b, 0);

    debug_assert_eq!(b.len(), code_off);
    b.extend_from_slice(text);

    while b.len() < reloc_off {
        b.push(0);
    }
    for &(addr, sym, rtype, pcrel) in relocs {
        put_u32(&mut b, addr);
        let packed = (sym & 0x00FF_FFFF)
            | ((pcrel as u32) << 24)
            | (2 << 25) // r_length = 2
            | (1 << 27) // r_extern = 1
            | (rtype << 28);
        put_u32(&mut b, packed);
    }

    while b.len() < sym_off {
        b.push(0);
    }
    for (i, (_, value)) in defined.iter().enumerate() {
        put_u32(&mut b, defined_strx[i]);
        b.push(0x0F); // N_SECT | N_EXT
        b.push(1);
        put_u16(&mut b, 0);
        put_u64(&mut b, *value);
    }
    for (i, (_, size, align_log2)) in commons.iter().enumerate() {
        put_u32(&mut b, common_strx[i]);
        b.push(0x01); // N_UNDF | N_EXT  (n_value=size => common/tentative)
        b.push(0);
        put_u16(&mut b, ((align_log2 & 0xF) << 8) as u16); // common alignment
        put_u64(&mut b, *size);
    }
    for &sx in &extern_strx {
        put_u32(&mut b, sx);
        b.push(0x01); // N_UNDF | N_EXT
        b.push(0);
        put_u16(&mut b, 0);
        put_u64(&mut b, 0);
    }

    debug_assert_eq!(b.len(), str_off);
    b.extend_from_slice(&strtab);
    b
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

fn align8(n: usize) -> usize {
    (n + 7) & !7
}
fn align16(n: u32) -> u32 {
    (n + 15) & !15
}
fn put_u16(b: &mut Vec<u8>, v: u16) {
    b.extend_from_slice(&v.to_le_bytes());
}
fn put_u32(b: &mut Vec<u8>, v: u32) {
    b.extend_from_slice(&v.to_le_bytes());
}
fn put_u64(b: &mut Vec<u8>, v: u64) {
    b.extend_from_slice(&v.to_le_bytes());
}
fn put_name16(b: &mut Vec<u8>, name: &str) {
    let mut field = [0u8; 16];
    let bytes = name.as_bytes();
    let n = bytes.len().min(16);
    field[..n].copy_from_slice(&bytes[..n]);
    b.extend_from_slice(&field);
}
