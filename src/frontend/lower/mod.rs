//! AST → SSA IR lowering — the final front-end step.
//!
//! Consumes the typed, laid-out [`Program`](crate::ast::Program) (so `e.ty()` is
//! populated and [`Layouts`] is available) and produces an [`IrProgram`] in SSA
//! form. SSA is built on the fly during the AST walk using the Braun et al. ("Simple
//! and Efficient Construction of Static Single Assignment Form", CC 2013)
//! `read_variable`/`write_variable` algorithm with sealed/unsealed blocks, so no
//! dominance-frontier computation is needed.
//!
//! Memory model (GCC's `is_gimple_reg` split): a local that is a scalar whose
//! address is never taken becomes an SSA variable; every other local (address-taken,
//! aggregate, array) gets a frame slot (`alloca`) and is reached through
//! [`Load`](crate::ir::IrInst::Load)/[`Store`](crate::ir::IrInst::Store). A per-function
//! pre-pass collects the address-taken names.
//!
//! Everything semantically subtle is decided **here**, once, from `e.ty()`:
//! narrow-int promote-then-truncate, signedness of `>>`/`/`/`%`/relationals,
//! float↔int conversion, pointer-arithmetic scaling, and store/arg/return coercion.
//! The IR then carries those decisions explicitly so the interpreter and both
//! backends cannot drift.
//!
//! This is built incrementally. The current slice covers scalar and memory locals
//! (pointers, arrays, classes, `&`/`*`/`[]`/`.`/`->`), brace/designated initializers,
//! control flow, and direct/primitive calls. Globals, string literals, by-value
//! aggregate call args/returns, varargs, exceptions, and `switch` report a clear
//! "not yet lowered" error until later increments.

use std::collections::{HashMap, HashSet};

use crate::ast::{
    AssignOp, BinOp, Expr, ExprKind, FuncDef, PostOp, Program, SizeofArg, Stmt, StmtKind, Type,
    UnOp,
};
use crate::backend::CodegenError;
use crate::ir::*;
use crate::layout::Layouts;

mod expr;
mod init;
mod ssa;
mod stmt;

/// Whether `ty` is an aggregate (a class/union or array) — represented by its address,
/// never a register, so it lives in an `alloca` slot reached via load/store.
fn is_aggregate(ty: &Type) -> bool {
    matches!(ty, Type::Named(_) | Type::Array(..))
}

/// Lower a typed, laid-out program to SSA IR.
///
/// Precondition: `sema::check_program` has run (every `Expr` is type-annotated) and
/// `layouts` is the result of `layout::compute` for `program`.
pub fn lower(program: &Program, layouts: &Layouts) -> Result<IrProgram, CodegenError> {
    let mut sigs: HashMap<String, FnSig> = HashMap::new();
    let mut defined: HashSet<String> = HashSet::new();
    for item in &program.items {
        if let StmtKind::Func(f) = &item.kind {
            sigs.insert(
                f.name.clone(),
                FnSig {
                    params: f.params.iter().map(|p| p.ty.clone()).collect(),
                    param_names: f.params.iter().map(|p| p.name.clone()).collect(),
                    defaults: f.params.iter().map(|p| p.default.clone()).collect(),
                    ret: f.ret.clone(),
                    varargs: f.varargs,
                },
            );
            if f.body.is_some() {
                defined.insert(f.name.clone());
            }
        }
    }

    // Register every top-level global variable, in declaration order, so any function
    // can resolve it. Globals live zero-initialised in the data region; a non-trivial
    // initializer runs as code in `@entry` at the point it appears, matching the
    // interpreter's top-level execution order. (The impure sema-injected globals
    // `argc`/`argv`/`envp` are intentionally not registered yet.)
    let mut out_globals: Vec<IrGlobal> = Vec::new();
    let mut globals: HashMap<String, (GlobalId, Type)> = HashMap::new();
    for item in &program.items {
        if let StmtKind::VarDecl { decls } = &item.kind {
            for d in decls {
                let gid = out_globals.len() as GlobalId;
                out_globals.push(IrGlobal {
                    name: d.name.clone(),
                    size: (layouts.size_of(&d.ty) as u32).max(1),
                    align: (layouts.align_of(&d.ty) as u32).max(1),
                    is_public: d.is_public,
                });
                globals.insert(d.name.clone(), (gid, d.ty.clone()));
            }
        }
    }

    // The sema-injected `Fs` (`CTask *`) holds the per-task exception state. Register
    // it as a global (the interpreter seeds it to point at a fresh `CTask` region) so
    // `try`/`catch`/`throw` and `Fs->…` resolve. Registered whenever `CTask` exists,
    // which the prelude guarantees — matching the interpreter, which materialises `Fs`
    // unconditionally.
    if layouts.get("CTask").is_some() && !globals.contains_key("Fs") {
        let gid = out_globals.len() as GlobalId;
        let fs_ty = Type::Ptr(Box::new(Type::Named("CTask".to_string())));
        out_globals.push(IrGlobal {
            name: "Fs".to_string(),
            size: 8,
            align: 8,
            is_public: true,
        });
        globals.insert("Fs".to_string(), (gid, fs_ty));
    }

    // The command line / environment, exposed as implicit globals captured at program
    // entry (`I64 argc`, `U8 **argv`, `U8 **envp`) — the same names sema injects. Each is
    // registered only when the program references it, so an arg-free program is unchanged.
    // `argc`/`argv` are command-line *only outside* a variadic function, where they are the
    // varargs that shadow these globals — so a `printf` caller does not drag in the command
    // line. `envp` is never shadowed, so an ordinary use check suffices. The native
    // `@entry` stores the incoming argc/argv/envp registers into these slots; the
    // interpreter seeds them in `fresh_mem`.
    let u8pp = || Type::Ptr(Box::new(Type::Ptr(Box::new(Type::U8))));
    let cmdline = crate::ast::program_uses_command_line(program, &["argc", "argv"]);
    let env = crate::ast::program_uses_ident(program, &["envp"]);
    for (name, ty, used) in [
        ("argc", Type::I64, cmdline),
        ("argv", u8pp(), cmdline),
        ("envp", u8pp(), env),
    ] {
        if used && !globals.contains_key(name) {
            let gid = out_globals.len() as GlobalId;
            out_globals.push(IrGlobal {
                name: name.to_string(),
                size: 8,
                align: 8,
                is_public: true,
            });
            globals.insert(name.to_string(), (gid, ty));
        }
    }

    // Top-level scalars that can become `@entry` SSA locals instead of globals: a scalar (so it
    // can be an SSA value), never `public` (a public global may be referenced from another
    // directory), never address-taken anywhere in the program (so no pointer can alias it), and
    // referenced only by top-level code (no function body uses it). Promoting such a loop
    // counter lets LICM / register promotion treat it as a value rather than a global reloaded
    // and rewritten every iteration. Semantics-preserving — and the golden suite checks it,
    // since the interpreter consumes this same lowered IR.
    let mut addr_taken_prog: HashSet<String> = HashSet::new();
    for item in &program.items {
        match &item.kind {
            StmtKind::Func(func) => {
                if let Some(b) = &func.body {
                    for s in b {
                        collect_addr_taken(s, &mut addr_taken_prog);
                    }
                }
            }
            _ => collect_addr_taken(item, &mut addr_taken_prog),
        }
    }
    let mut promotable: HashSet<String> = HashSet::new();
    for item in &program.items {
        if let StmtKind::VarDecl { decls } = &item.kind {
            for d in decls {
                if scalar_ir_ty(&d.ty).is_some()
                    && !d.is_public
                    && !addr_taken_prog.contains(&d.name)
                    && !crate::ast::global_used_in_functions(program, &d.name)
                {
                    promotable.insert(d.name.clone());
                }
            }
        }
    }

    let mut strings = StringInterner::default();
    let mut funcs = Vec::new();

    // Lower each defined top-level function. During incremental bring-up a function
    // that uses a not-yet-lowered construct is skipped, not fatal — the interpreter
    // errors only if such a function is actually called, so the supported subset of
    // any program still runs.
    for item in &program.items {
        if let StmtKind::Func(f) = &item.kind {
            if let Some(body) = &f.body {
                let mut lw = Lowerer::new(
                    layouts,
                    &sigs,
                    &defined,
                    &globals,
                    &mut strings,
                    f,
                    false,
                    &promotable,
                );
                match lw.lower_func(f, body) {
                    Ok(irf) => funcs.push(irf),
                    Err(e) => debug_skip(&f.name, &e),
                }
            }
        }
    }

    // Synthesise an `@entry` function from the top-level code (the program "main"),
    // mirroring how the interpreter executes top-level statements in order.
    let top: Vec<Stmt> = program
        .items
        .iter()
        .filter(|s| {
            !matches!(
                s.kind,
                StmtKind::Func(_) | StmtKind::Class(_) | StmtKind::Include(_) | StmtKind::Empty
            )
        })
        .cloned()
        .collect();
    // Always synthesise `@entry`, even with no top-level code, so every program has
    // an entry function (the backends emit it as `_main`). It returns `I64` — the
    // process exit code — so a top-level `return N;` exits with `N` and a fall-off
    // returns 0, matching a C `main`. (The interpreter oracle captures stdout and
    // ignores this return, so it stays byte-for-byte; only the native exit status uses
    // it.)
    {
        let entry = FuncDef {
            ret: Type::I64,
            name: ENTRY.to_string(),
            params: Vec::new(),
            varargs: false,
            body: Some(top),
            is_public: false,
        };
        let body = entry.body.clone().unwrap();
        let mut lw = Lowerer::new(
            layouts,
            &sigs,
            &defined,
            &globals,
            &mut strings,
            &entry,
            true,
            &promotable,
        );
        match lw.lower_func(&entry, &body) {
            Ok(irf) => funcs.push(irf),
            Err(e) => debug_skip(ENTRY, &e),
        }
    }

    Ok(IrProgram {
        funcs,
        globals: out_globals,
        strings: strings.table,
        layouts: layouts.clone(),
    })
}

/// The synthetic name of the top-level-code entry function.
pub const ENTRY: &str = "@entry";

/// Size of an on-stack exception frame slot: `{ prev, saved_sp, saved_fp, landing_pad }`
/// (4×8). The interpreter ignores its contents (it unwinds via the try-region stack);
/// the native backends populate it. The spill-everything IR backends keep no values in
/// callee-saved registers across a `try`, so — unlike the AST backend — there is no
/// callee-saved set to save/restore here.
const EXC_FRAME_SIZE: u32 = 32;

/// Report a skipped (not-yet-lowerable) function when `IR_LOWER_DEBUG` is set, so the
/// differential gate can show which construct each unsupported function needs.
fn debug_skip(name: &str, err: &CodegenError) {
    if std::env::var_os("IR_LOWER_DEBUG").is_some() {
        eprintln!("LOWER-SKIP {name}: {err}");
    }
}

/// Interns string literals to stable [`StrId`]s, deduplicating identical bytes so a
/// repeated literal has one stable address.
#[derive(Default)]
struct StringInterner {
    table: Vec<Vec<u8>>,
    ids: HashMap<Vec<u8>, StrId>,
}

impl StringInterner {
    /// Intern a string's bytes (NUL-terminated, as C strings are).
    fn intern(&mut self, s: &str) -> StrId {
        let mut bytes = s.as_bytes().to_vec();
        bytes.push(0);
        if let Some(id) = self.ids.get(&bytes) {
            return *id;
        }
        let id = self.table.len() as StrId;
        self.ids.insert(bytes.clone(), id);
        self.table.push(bytes);
        id
    }
}

/// A function's call-relevant signature, for argument classing and the result type.
struct FnSig {
    params: Vec<Type>,
    /// Parameter names, so a default expression can reference an earlier parameter.
    param_names: Vec<Option<String>>,
    /// Per-parameter default value expression, for calls that omit trailing args.
    defaults: Vec<Option<Expr>>,
    ret: Type,
    varargs: bool,
}

/// A variable's identity, independent of its (possibly shadowed) name.
type VarId = u32;

/// Where a variable lives: an SSA value, or a frame slot (memory).
#[derive(Clone, Copy)]
enum Place {
    Ssa(VarId),
    Mem(SlotId),
}

/// Per-variable lowering info.
struct VarInfo {
    ty: Type,
    place: Place,
}

/// An lvalue resolved for read/modify/write: either an SSA variable or a memory
/// address with its pointee AST type.
enum LValue {
    Ssa { id: VarId, ast: Type },
    Mem { addr: Val, ast: Type },
}

struct Lowerer<'a> {
    layouts: &'a Layouts,
    sigs: &'a HashMap<String, FnSig>,
    defined: &'a HashSet<String>,
    globals: &'a HashMap<String, (GlobalId, Type)>,
    strings: &'a mut StringInterner,
    /// True when lowering the synthesised top-level `@entry`, where a top-level
    /// `VarDecl` defines a global rather than a local.
    is_entry: bool,
    /// Top-level scalar names that `@entry` lowers as SSA locals instead of globals (computed
    /// whole-program; see `lower`). Consulted only in `@entry`'s outermost scope.
    promotable: &'a HashSet<String>,
    /// True when the function uses `try`/`throw`/`Fs`: locals are forced to memory so
    /// they stay live across an exception unwind (the handler is reached by a non-local
    /// jump, not a CFG edge). Matches the native "no register promotion in try
    /// functions" rule.
    force_mem: bool,
    ret_ty: Type,
    addr_taken: HashSet<String>,

    blocks: Vec<IrBlock>,
    sealed: Vec<bool>,
    preds: Vec<Vec<BlockId>>,
    incomplete: Vec<Vec<(VarId, Vreg)>>,
    current_def: HashMap<(VarId, BlockId), Val>,
    var_ty: Vec<IrTy>,
    slots: Vec<SlotInfo>,

    scopes: Vec<HashMap<String, VarInfo>>,
    next_vreg: u32,
    next_var: u32,

    cur: BlockId,
    cur_terminated: bool,
    /// Loop/switch exit and continue targets, each paired with the `try`-nesting depth
    /// at which the loop was entered — so `break`/`continue` can pop the exception frames
    /// of any `try` regions they escape (`TryEnd` per escaped region).
    break_targets: Vec<(BlockId, usize)>,
    continue_targets: Vec<(BlockId, usize)>,
    labels: HashMap<String, BlockId>,
    /// `try`-nesting depth at each lowered label, so a backward `goto` out of a `try`
    /// pops the escaped frames too.
    label_try_depth: HashMap<String, usize>,
    /// Number of currently-active `try` regions (pushed by `TryBegin`, popped by
    /// `TryEnd`); the live exception-frame chain depth at the current program point.
    try_depth: usize,
}

impl<'a> Lowerer<'a> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        layouts: &'a Layouts,
        sigs: &'a HashMap<String, FnSig>,
        defined: &'a HashSet<String>,
        globals: &'a HashMap<String, (GlobalId, Type)>,
        strings: &'a mut StringInterner,
        f: &FuncDef,
        is_entry: bool,
        promotable: &'a HashSet<String>,
    ) -> Self {
        let mut addr_taken = HashSet::new();
        let mut force_mem = false;
        if let Some(body) = &f.body {
            for s in body {
                collect_addr_taken(s, &mut addr_taken);
            }
            let refs: Vec<&Stmt> = body.iter().collect();
            force_mem = crate::ast::stmts_use_fs_or_exceptions(&refs);
        }
        Lowerer {
            layouts,
            sigs,
            defined,
            globals,
            strings,
            is_entry,
            promotable,
            force_mem,
            ret_ty: f.ret.clone(),
            addr_taken,
            blocks: Vec::new(),
            sealed: Vec::new(),
            preds: Vec::new(),
            incomplete: Vec::new(),
            current_def: HashMap::new(),
            var_ty: Vec::new(),
            slots: Vec::new(),
            scopes: vec![HashMap::new()],
            next_vreg: 0,
            next_var: 0,
            cur: 0,
            cur_terminated: false,
            break_targets: Vec::new(),
            continue_targets: Vec::new(),
            labels: HashMap::new(),
            label_try_depth: HashMap::new(),
            try_depth: 0,
        }
    }

    fn lower_func(&mut self, f: &FuncDef, body: &[Stmt]) -> Result<IrFunc, CodegenError> {
        let entry = self.new_block();
        self.seal_block(entry);
        self.cur = entry;

        let mut params = Vec::new();

        // A by-value aggregate return is delivered through a hidden leading `$sret`
        // pointer parameter; `return aggexpr;` copies into it.
        if matches!(ret_of(&f.ret, self.layouts), IrRet::Agg { .. }) {
            let vreg = self.fresh_vreg();
            params.push(IrParam {
                ty: ArgTy::Int(IrTy::Ptr),
                vreg,
                name: Some("$sret".to_string()),
            });
            let id = self.bind_ssa("$sret", Type::Ptr(Box::new(Type::U0)));
            self.write_variable(id, entry, Val::Reg(vreg));
        }

        for p in &f.params {
            let name = p.name.clone();
            // An array parameter decays to a pointer to its data (by reference): the
            // incoming value IS that pointer, held in an SSA register and read as the
            // "array data address" wherever the array name is used.
            if matches!(p.ty, Type::Array(..)) {
                let vreg = self.fresh_vreg();
                params.push(IrParam {
                    ty: ArgTy::Int(IrTy::Ptr),
                    vreg,
                    name: name.clone(),
                });
                if let Some(name) = name {
                    let id = self.alloc_var(IrTy::Ptr);
                    self.bind(&name, p.ty.clone(), Place::Ssa(id));
                    self.write_variable(id, entry, Val::Reg(vreg));
                }
                continue;
            }
            // A class/union parameter is passed by value, carried by address; the
            // callee copies it into its own slot.
            if is_aggregate(&p.ty) {
                let size = self.layouts.size_of(&p.ty) as u32;
                let align = self.layouts.align_of(&p.ty) as u32;
                let vreg = self.fresh_vreg();
                params.push(IrParam {
                    ty: ArgTy::AggAddr { size, align },
                    vreg,
                    name: name.clone(),
                });
                if let Some(name) = name {
                    let slot = self.add_slot(size, align, SlotKind::Param, Some(name.clone()));
                    let dst = self.slot_addr(slot, 0);
                    self.emit(IrInst::MemCpy {
                        dst,
                        src: Val::Reg(vreg),
                        len: size,
                    });
                    self.bind(&name, p.ty.clone(), Place::Mem(slot));
                }
                continue;
            }

            let pty = scalar_ir_ty(&p.ty)
                .ok_or_else(|| CodegenError::at(p.span.pos, "non-scalar parameter not lowered"))?;
            let vreg = self.fresh_vreg();
            params.push(IrParam {
                ty: if pty.is_float() {
                    ArgTy::Float
                } else {
                    ArgTy::Int(pty)
                },
                vreg,
                name: name.clone(),
            });
            if let Some(name) = name {
                if self.force_mem || self.addr_taken.contains(&name) {
                    let slot =
                        self.add_slot(pty.size(), pty.size(), SlotKind::Param, Some(name.clone()));
                    let addr = self.slot_addr(slot, 0);
                    self.emit(IrInst::Store {
                        ty: pty,
                        addr,
                        val: Val::Reg(vreg),
                    });
                    self.bind(&name, p.ty.clone(), Place::Mem(slot));
                } else {
                    let id = self.bind_ssa(&name, p.ty.clone());
                    self.write_variable(id, entry, Val::Reg(vreg));
                }
            }
        }

        // A variadic function receives two hidden trailing parameters: `argc` (the
        // count) and `argv` (a pointer to the packed argument buffer). They are
        // sema-injected locals in the body; bind them here as SSA values.
        if f.varargs {
            let vc = self.fresh_vreg();
            params.push(IrParam {
                ty: ArgTy::Int(IrTy::I64),
                vreg: vc,
                name: Some("argc".to_string()),
            });
            let vc_id = self.bind_ssa("argc", Type::I64);
            self.write_variable(vc_id, entry, Val::Reg(vc));

            let vv = self.fresh_vreg();
            params.push(IrParam {
                ty: ArgTy::Int(IrTy::Ptr),
                vreg: vv,
                name: Some("argv".to_string()),
            });
            let vv_id = self.bind_ssa("argv", Type::Ptr(Box::new(Type::I64)));
            self.write_variable(vv_id, entry, Val::Reg(vv));
        }

        for s in body {
            self.lower_stmt(s)?;
        }
        if !self.cur_terminated {
            let t = self.default_ret();
            self.terminate(t);
        }
        let label_blocks: Vec<BlockId> = self.labels.values().copied().collect();
        for b in label_blocks {
            if !self.sealed[b as usize] {
                self.seal_block(b);
            }
        }

        Ok(IrFunc {
            name: f.name.clone(),
            ret: ret_of(&f.ret, self.layouts),
            params,
            varargs: f.varargs,
            slots: std::mem::take(&mut self.slots),
            blocks: std::mem::take(&mut self.blocks),
            entry,
            n_vregs: self.next_vreg,
        })
    }

    fn default_ret(&self) -> IrTerm {
        match ret_of(&self.ret_ty, self.layouts) {
            IrRet::Void | IrRet::Agg { .. } => IrTerm::Ret(None),
            IrRet::Scalar(t) if t.is_float() => IrTerm::Ret(Some(Val::ImmF64(0))),
            IrRet::Scalar(_) => IrTerm::Ret(Some(Val::ImmInt(0))),
        }
    }

    // ---- id / block / slot management ----

    fn fresh_vreg(&mut self) -> Vreg {
        let v = self.next_vreg;
        self.next_vreg += 1;
        v
    }

    fn alloc_var(&mut self, ty: IrTy) -> VarId {
        let id = self.next_var;
        self.next_var += 1;
        self.var_ty.push(ty);
        id
    }

    fn add_slot(&mut self, size: u32, align: u32, kind: SlotKind, name: Option<String>) -> SlotId {
        let id = self.slots.len() as SlotId;
        self.slots.push(SlotInfo {
            size: size.max(1),
            align: align.max(1),
            kind,
            name,
        });
        id
    }

    fn bind(&mut self, name: &str, ty: Type, place: Place) {
        self.scopes
            .last_mut()
            .unwrap()
            .insert(name.to_string(), VarInfo { ty, place });
    }

    fn bind_ssa(&mut self, name: &str, ty: Type) -> VarId {
        let irty = scalar_ir_ty(&ty).unwrap_or(IrTy::I64);
        let id = self.alloc_var(irty);
        self.bind(name, ty, Place::Ssa(id));
        id
    }

    fn lookup(&self, name: &str) -> Option<&VarInfo> {
        self.scopes.iter().rev().find_map(|s| s.get(name))
    }

    fn new_block(&mut self) -> BlockId {
        let id = self.blocks.len() as BlockId;
        self.blocks.push(IrBlock {
            id,
            phis: Vec::new(),
            insts: Vec::new(),
            term: IrTerm::Unreachable,
        });
        self.sealed.push(false);
        self.preds.push(Vec::new());
        self.incomplete.push(Vec::new());
        id
    }

    fn emit(&mut self, inst: IrInst) {
        debug_assert!(!self.cur_terminated, "emit into a terminated block");
        let cur = self.cur as usize;
        self.blocks[cur].insts.push(inst);
    }

    fn terminate(&mut self, term: IrTerm) {
        debug_assert!(!self.cur_terminated);
        let cur = self.cur;
        for s in term.successors() {
            self.preds[s as usize].push(cur);
        }
        self.blocks[cur as usize].term = term;
        self.cur_terminated = true;
    }

    fn switch_to(&mut self, b: BlockId) {
        self.cur = b;
        self.cur_terminated = false;
    }

    fn ensure_live(&mut self) {
        if self.cur_terminated {
            let b = self.new_block();
            self.seal_block(b);
            self.switch_to(b);
        }
    }

    fn slot_addr(&mut self, slot: SlotId, off: u32) -> Val {
        let dst = self.fresh_vreg();
        self.emit(IrInst::SlotAddr { dst, slot, off });
        Val::Reg(dst)
    }

    fn global_addr(&mut self, global: GlobalId, off: u32) -> Val {
        let dst = self.fresh_vreg();
        self.emit(IrInst::GlobalAddr { dst, global, off });
        Val::Reg(dst)
    }

    /// `base + off` bytes, folding a zero offset away.
    fn offset_addr(&mut self, base: Val, off: u32) -> Val {
        if off == 0 {
            return base;
        }
        let dst = self.fresh_vreg();
        self.emit(IrInst::PtrAdd {
            dst,
            base,
            index: Val::ImmInt(off as i64),
            stride: 1,
        });
        Val::Reg(dst)
    }

    // ---- small helpers ----

    fn expr_ty(&self, e: &Expr) -> IrTy {
        e.ty().as_ref().and_then(scalar_ir_ty).unwrap_or(IrTy::I64)
    }

    fn field_ty(&self, class: &str, field: &str) -> Option<Type> {
        self.layouts
            .get(class)?
            .fields
            .iter()
            .find(|f| f.name == field)
            .map(|f| f.ty.clone())
    }

    /// `(offset, type)` of each field of a class, in order (for positional init).
    fn aggregate_fields(&self, class: &str) -> Vec<(u32, Type)> {
        self.layouts
            .get(class)
            .map(|l| {
                l.fields
                    .iter()
                    .map(|f| (f.offset as u32, f.ty.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }
}

// ---- shared lowering helpers ----

fn scalar_ir_ty(ty: &Type) -> Option<IrTy> {
    Some(match ty {
        Type::I8 => IrTy::I8,
        Type::U8 | Type::Bool => IrTy::U8,
        Type::I16 => IrTy::I16,
        Type::U16 => IrTy::U16,
        Type::I32 => IrTy::I32,
        Type::U32 => IrTy::U32,
        Type::I64 => IrTy::I64,
        Type::U64 => IrTy::U64,
        Type::F64 => IrTy::F64,
        Type::Ptr(_) | Type::FuncPtr { .. } => IrTy::Ptr,
        _ => return None,
    })
}

fn ret_of(ty: &Type, layouts: &Layouts) -> IrRet {
    match ty {
        Type::U0 => IrRet::Void,
        Type::Named(_) | Type::Array(..) => IrRet::Agg {
            size: layouts.size_of(ty) as u32,
            align: layouts.align_of(ty) as u32,
        },
        other => match scalar_ir_ty(other) {
            Some(t) => IrRet::Scalar(t),
            None => IrRet::Void,
        },
    }
}

/// Collect the names whose address is taken (`&name`), which forces a scalar local
/// out of SSA and into a frame slot. Conservative across shadowing (a name taken in
/// any scope marks all same-named locals memory-backed), which is safe.
fn collect_addr_taken(s: &Stmt, out: &mut HashSet<String>) {
    fn ex(e: &Expr, out: &mut HashSet<String>) {
        match &e.kind {
            ExprKind::Unary {
                op: UnOp::AddrOf,
                expr,
            } => {
                if let ExprKind::Ident(n) = &expr.kind {
                    out.insert(n.clone());
                }
                ex(expr, out);
            }
            ExprKind::Unary { expr, .. } | ExprKind::Postfix { expr, .. } => ex(expr, out),
            ExprKind::Cast { expr, .. } => ex(expr, out),
            ExprKind::Binary { lhs, rhs, .. } => {
                ex(lhs, out);
                ex(rhs, out);
            }
            ExprKind::Assign { target, value, .. } => {
                ex(target, out);
                ex(value, out);
            }
            ExprKind::Ternary { cond, then, else_ } => {
                ex(cond, out);
                ex(then, out);
                ex(else_, out);
            }
            ExprKind::Call { callee, args } => {
                ex(callee, out);
                args.iter().for_each(|a| ex(a, out));
            }
            ExprKind::Index { base, index } => {
                ex(base, out);
                ex(index, out);
            }
            ExprKind::Member { base, .. } => ex(base, out),
            ExprKind::InitList(es) | ExprKind::Comma(es) => es.iter().for_each(|e| ex(e, out)),
            ExprKind::DesignatedInit(fs) => fs.iter().for_each(|(_, e)| ex(e, out)),
            _ => {}
        }
    }
    fn st(s: &Stmt, out: &mut HashSet<String>) {
        match &s.kind {
            StmtKind::Expr(e) | StmtKind::Throw(Some(e)) => ex(e, out),
            StmtKind::VarDecl { decls } => {
                for d in decls {
                    if let Some(i) = &d.init {
                        ex(i, out);
                    }
                }
            }
            StmtKind::Block(ss) => ss.iter().for_each(|s| st(s, out)),
            StmtKind::If { cond, then, else_ } => {
                ex(cond, out);
                st(then, out);
                if let Some(e) = else_ {
                    st(e, out);
                }
            }
            StmtKind::While { cond, body } | StmtKind::Switch { cond, body } => {
                ex(cond, out);
                st(body, out);
            }
            StmtKind::DoWhile { body, cond } => {
                st(body, out);
                ex(cond, out);
            }
            StmtKind::For {
                init,
                cond,
                step,
                body,
            } => {
                if let Some(i) = init {
                    st(i, out);
                }
                if let Some(c) = cond {
                    ex(c, out);
                }
                if let Some(s) = step {
                    ex(s, out);
                }
                st(body, out);
            }
            StmtKind::Case { lo, hi } => {
                ex(lo, out);
                if let Some(h) = hi {
                    ex(h, out);
                }
            }
            StmtKind::Return(Some(e)) => ex(e, out),
            StmtKind::Try { body, handler } => {
                body.iter().for_each(|s| st(s, out));
                handler.iter().for_each(|s| st(s, out));
            }
            _ => {}
        }
    }
    st(s, out);
}
