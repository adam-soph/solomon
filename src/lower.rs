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
use crate::codegen::CodegenError;
use crate::ir::*;
use crate::layout::Layouts;

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
    // `ArgC`/`ArgV`/`EnvP` are intentionally not registered yet.)
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
                    init: Vec::new(),
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
            init: Vec::new(),
        });
        globals.insert("Fs".to_string(), (gid, fs_ty));
    }

    // The command line / environment, exposed as implicit globals captured at program
    // entry (`I64 ArgC`, `U8 **ArgV`, `U8 **EnvP`) — the same names sema injects. Each
    // is registered only when the program references it, so an arg-free program is
    // unchanged. The native `@entry` stores the incoming argc/argv/envp registers into
    // these slots; the interpreter seeds them in `fresh_mem`.
    let u8pp = || Type::Ptr(Box::new(Type::Ptr(Box::new(Type::U8))));
    for (name, ty) in [("ArgC", Type::I64), ("ArgV", u8pp()), ("EnvP", u8pp())] {
        if crate::ast::program_uses_ident(program, &[name]) && !globals.contains_key(name) {
            let gid = out_globals.len() as GlobalId;
            out_globals.push(IrGlobal {
                name: name.to_string(),
                size: 8,
                align: 8,
                is_public: true,
                init: Vec::new(),
            });
            globals.insert(name.to_string(), (gid, ty));
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
                let mut lw =
                    Lowerer::new(layouts, &sigs, &defined, &globals, &mut strings, f, false);
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

        // A variadic function receives two hidden trailing parameters: `VargC` (the
        // count) and `VargV` (a pointer to the packed argument buffer). They are
        // sema-injected locals in the body; bind them here as SSA values.
        if f.varargs {
            let vc = self.fresh_vreg();
            params.push(IrParam {
                ty: ArgTy::Int(IrTy::I64),
                vreg: vc,
                name: Some("VargC".to_string()),
            });
            let vc_id = self.bind_ssa("VargC", Type::I64);
            self.write_variable(vc_id, entry, Val::Reg(vc));

            let vv = self.fresh_vreg();
            params.push(IrParam {
                ty: ArgTy::Int(IrTy::Ptr),
                vreg: vv,
                name: Some("VargV".to_string()),
            });
            let vv_id = self.bind_ssa("VargV", Type::Ptr(Box::new(Type::I64)));
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

    // ---- Braun SSA construction ----

    fn write_variable(&mut self, var: VarId, block: BlockId, val: Val) {
        self.current_def.insert((var, block), val);
    }

    fn read_variable(&mut self, var: VarId, block: BlockId) -> Val {
        if let Some(v) = self.current_def.get(&(var, block)) {
            return *v;
        }
        self.read_variable_recursive(var, block)
    }

    fn read_variable_recursive(&mut self, var: VarId, block: BlockId) -> Val {
        let ty = self.var_ty[var as usize];
        let val = if !self.sealed[block as usize] {
            let phi = self.fresh_vreg();
            self.blocks[block as usize].phis.push(Phi {
                dst: phi,
                ty,
                args: Vec::new(),
            });
            self.incomplete[block as usize].push((var, phi));
            Val::Reg(phi)
        } else if self.preds[block as usize].len() == 1 {
            let p = self.preds[block as usize][0];
            self.read_variable(var, p)
        } else {
            let phi = self.fresh_vreg();
            self.blocks[block as usize].phis.push(Phi {
                dst: phi,
                ty,
                args: Vec::new(),
            });
            self.write_variable(var, block, Val::Reg(phi));
            self.add_phi_operands(var, block, phi);
            Val::Reg(phi)
        };
        self.write_variable(var, block, val);
        val
    }

    fn add_phi_operands(&mut self, var: VarId, block: BlockId, phi: Vreg) {
        let preds = self.preds[block as usize].clone();
        let mut args = Vec::with_capacity(preds.len());
        for p in preds {
            let v = self.read_variable(var, p);
            args.push((p, v));
        }
        if let Some(ph) = self.blocks[block as usize]
            .phis
            .iter_mut()
            .find(|ph| ph.dst == phi)
        {
            ph.args = args;
        }
    }

    fn seal_block(&mut self, block: BlockId) {
        let pending = std::mem::take(&mut self.incomplete[block as usize]);
        for (var, phi) in pending {
            self.add_phi_operands(var, block, phi);
        }
        self.sealed[block as usize] = true;
    }

    // ---- statements ----

    fn lower_stmt(&mut self, s: &Stmt) -> Result<(), CodegenError> {
        self.ensure_live();
        match &s.kind {
            StmtKind::Empty | StmtKind::Include(_) => Ok(()),
            StmtKind::Expr(e) => self.lower_stmt_expr(e),
            StmtKind::Block(ss) => {
                self.scopes.push(HashMap::new());
                for s in ss {
                    self.lower_stmt(s)?;
                }
                self.scopes.pop();
                Ok(())
            }
            StmtKind::VarDecl { decls } => {
                for d in decls {
                    self.lower_decl(&d.name, &d.ty, d.init.as_ref())?;
                }
                Ok(())
            }
            StmtKind::If { cond, then, else_ } => self.lower_if(cond, then, else_.as_deref()),
            StmtKind::While { cond, body } => self.lower_while(cond, body),
            StmtKind::DoWhile { body, cond } => self.lower_do_while(body, cond),
            StmtKind::For {
                init,
                cond,
                step,
                body,
            } => self.lower_for(init.as_deref(), cond.as_ref(), step.as_ref(), body),
            StmtKind::Switch { cond, body } => self.lower_switch(cond, body, s.span.pos),
            StmtKind::Return(e) => {
                let term = match e {
                    Some(e) => match ret_of(&self.ret_ty, self.layouts) {
                        IrRet::Void => {
                            self.lower_expr_discard(e)?;
                            IrTerm::Ret(None)
                        }
                        IrRet::Agg { size, .. } => {
                            // Copy the aggregate into the caller-provided `$sret` slot.
                            let src = self.lower_aggregate_addr(e)?;
                            let id = self
                                .lookup("$sret")
                                .and_then(|i| match i.place {
                                    Place::Ssa(id) => Some(id),
                                    Place::Mem(_) => None,
                                })
                                .expect("aggregate-returning function has $sret");
                            let cur = self.cur;
                            let dst = self.read_variable(id, cur);
                            self.emit(IrInst::MemCpy {
                                dst,
                                src,
                                len: size,
                            });
                            IrTerm::Ret(None)
                        }
                        IrRet::Scalar(rt) => {
                            let (v, vt) = self.lower_expr(e)?;
                            let v = self.coerce(v, vt, rt);
                            IrTerm::Ret(Some(v))
                        }
                    },
                    None => IrTerm::Ret(None),
                };
                self.terminate(term);
                Ok(())
            }
            StmtKind::Break => {
                let &(t, depth) = self
                    .break_targets
                    .last()
                    .ok_or_else(|| CodegenError::at(s.span.pos, "break outside a loop"))?;
                self.exit_try_regions(depth);
                self.terminate(IrTerm::Br(t));
                Ok(())
            }
            StmtKind::Continue => {
                let &(t, depth) = self
                    .continue_targets
                    .last()
                    .ok_or_else(|| CodegenError::at(s.span.pos, "continue outside a loop"))?;
                self.exit_try_regions(depth);
                self.terminate(IrTerm::Br(t));
                Ok(())
            }
            StmtKind::Label(name) => {
                let b = self.label_block(name);
                self.label_try_depth.insert(name.clone(), self.try_depth);
                self.terminate(IrTerm::Br(b));
                self.switch_to(b);
                Ok(())
            }
            StmtKind::Goto(name) => {
                let b = self.label_block(name);
                // A backward `goto` out of a `try` pops the escaped frames; a forward
                // `goto`'s target depth is not yet known, so it is assumed to be the
                // same `try` level (correct for the common within-loop/within-`try` jump).
                if let Some(&depth) = self.label_try_depth.get(name) {
                    self.exit_try_regions(depth);
                }
                self.terminate(IrTerm::Br(b));
                Ok(())
            }
            StmtKind::Try { body, handler } => self.lower_try(body, handler),
            StmtKind::Throw(val) => self.lower_throw(val.as_ref()),
            other => Err(CodegenError::at(
                s.span.pos,
                format!("statement not yet lowered: {}", stmt_name(other)),
            )),
        }
    }

    /// Emit a `TryEnd` for each `try` region between the current depth and `target_depth`
    /// (exclusive of `target_depth`): the frames a non-local exit (`break`/`continue`/
    /// `goto`) escapes. The lexical `try_depth` is unchanged — this only pops frames on
    /// the escaping control-flow edge; the fall-through path keeps the regions active.
    fn exit_try_regions(&mut self, target_depth: usize) {
        for _ in target_depth..self.try_depth {
            self.emit(IrInst::TryEnd);
        }
    }

    /// Lower `try { body } catch { handler }`. The handler block (the landing pad) is
    /// reached by the exception unwind, not a CFG edge, so it has no normal
    /// predecessor; `force_mem` keeps cross-`try` locals in memory so the handler can
    /// read them. On normal completion the `try` region is popped (`TryEnd`); a normal
    /// handler completion clears `Fs->catch_except`.
    fn lower_try(&mut self, body: &[Stmt], handler: &[Stmt]) -> Result<(), CodegenError> {
        let pad = self.new_block();
        let after = self.new_block();
        let frame = self.add_slot(EXC_FRAME_SIZE, 8, SlotKind::ExcFrame, None);
        self.emit(IrInst::TryBegin { pad, frame });

        // The region is active for the duration of the body; `break`/`continue`/`goto`
        // that escape it pop its frame via `exit_try_regions` (see above).
        self.try_depth += 1;
        self.scopes.push(HashMap::new());
        for s in body {
            self.lower_stmt(s)?;
        }
        self.scopes.pop();
        self.try_depth -= 1;
        if !self.cur_terminated {
            self.emit(IrInst::TryEnd);
            self.terminate(IrTerm::Br(after));
        }

        // The landing pad: reached by an unwind, so it has no CFG predecessor.
        self.seal_block(pad);
        self.switch_to(pad);
        self.scopes.push(HashMap::new());
        for s in handler {
            self.lower_stmt(s)?;
        }
        self.scopes.pop();
        if !self.cur_terminated {
            // The handler finished normally: clear the in-flight flag, then continue.
            self.store_fs_field("catch_except", Val::ImmInt(0))?;
            self.terminate(IrTerm::Br(after));
        }

        self.seal_block(after);
        self.switch_to(after);
        Ok(())
    }

    /// Lower `throw expr;` / bare `throw;` (re-raise). The value (coerced to `I64`) and
    /// the in-flight flag are written into `Fs` before the unwinding terminator.
    fn lower_throw(&mut self, val: Option<&Expr>) -> Result<(), CodegenError> {
        match val {
            Some(e) => {
                let (v, vt) = self.lower_expr(e)?;
                let v = self.coerce(v, vt, IrTy::I64);
                self.store_fs_field("except_ch", v)?;
                self.store_fs_field("catch_except", Val::ImmInt(1))?;
                self.terminate(IrTerm::Throw(v));
            }
            None => {
                // A bare `throw;` re-raises the current `Fs->except_ch`.
                self.store_fs_field("catch_except", Val::ImmInt(1))?;
                self.terminate(IrTerm::Rethrow);
            }
        }
        Ok(())
    }

    /// The address of `Fs->field` (load the `Fs` pointer, add the field offset).
    fn fs_field_addr(&mut self, field: &str) -> Result<Val, CodegenError> {
        let gid = self
            .globals
            .get("Fs")
            .map(|(g, _)| *g)
            .ok_or_else(|| CodegenError::new("Fs is not available", None))?;
        let fs_global = self.global_addr(gid, 0);
        let dst = self.fresh_vreg();
        self.emit(IrInst::Load {
            dst,
            ty: IrTy::Ptr,
            addr: fs_global,
        });
        let off = self
            .layouts
            .offset_of("CTask", field)
            .ok_or_else(|| CodegenError::new(format!("CTask has no field {field}"), None))?
            as u32;
        Ok(self.offset_addr(Val::Reg(dst), off))
    }

    fn store_fs_field(&mut self, field: &str, val: Val) -> Result<(), CodegenError> {
        let addr = self.fs_field_addr(field)?;
        self.emit(IrInst::Store {
            ty: IrTy::I64,
            addr,
            val,
        });
        Ok(())
    }

    fn lower_decl(
        &mut self,
        name: &str,
        ty: &Type,
        init: Option<&Expr>,
    ) -> Result<(), CodegenError> {
        // A variable-length array (a non-constant dimension) needs a dynamic stack
        // allocation, which is not yet lowered — bail so the caller can fall back.
        if let Type::Array(_, Some(dim)) = ty {
            if crate::layout::const_eval(dim).is_err() {
                return Err(CodegenError::new(
                    "variable-length array not yet lowered",
                    None,
                ));
            }
        }

        // A top-level `VarDecl` in `@entry` defines a global: initialise it in place
        // (the data region is zeroed) rather than declaring a local. This applies only at
        // `@entry`'s outermost scope (`scopes.len() == 1`); a same-named declaration in a
        // nested block is a genuine local that shadows the global, so it must fall through
        // to `declare_local` (otherwise `{ I64 x = 9; }` would clobber a global `x`).
        if self.is_entry && self.scopes.len() == 1 {
            if let Some(&(gid, _)) = self.globals.get(name) {
                let base = self.global_addr(gid, 0);
                self.init_memory(base, ty, init)?;
                return Ok(());
            }
        }

        // The initializer is lowered **before** the new name is bound, so a same-named
        // outer variable referenced in the initializer (`I64 v = v + 1;`,
        // `for (I64 i = i; …)`) resolves to that outer variable — matching the
        // tree-walking interpreter, which evaluates the init expression in the enclosing
        // scope and only then declares the new local.
        let use_mem = self.force_mem || is_aggregate(ty) || self.addr_taken.contains(name);
        if use_mem {
            let size = self.layouts.size_of(ty) as u32;
            let align = self.layouts.align_of(ty) as u32;
            let slot = self.add_slot(size, align, SlotKind::Local, Some(name.to_string()));
            let base = self.slot_addr(slot, 0);
            self.init_memory(base, ty, init)?;
            self.bind(name, ty.clone(), Place::Mem(slot));
        } else {
            let irty = scalar_ir_ty(ty).unwrap_or(IrTy::I64);
            let val = match init {
                Some(init) => {
                    let (v, vt) = self.lower_expr(init)?;
                    self.coerce_to_ast(v, vt, ty)?
                }
                None if irty.is_float() => Val::ImmF64(0),
                None => Val::ImmInt(0),
            };
            let id = self.bind_ssa(name, ty.clone());
            let cur = self.cur;
            self.write_variable(id, cur, val);
        }
        Ok(())
    }

    /// Zero a memory object at `base` (a slot or global), then apply `init` if any.
    fn init_memory(
        &mut self,
        base: Val,
        ty: &Type,
        init: Option<&Expr>,
    ) -> Result<(), CodegenError> {
        let size = self.layouts.size_of(ty) as u32;
        // Locals/globals are zero-initialised, so a missing or partial initializer
        // leaves the rest zero.
        self.emit(IrInst::MemZero {
            dst: base,
            len: size,
        });
        let Some(init) = init else { return Ok(()) };
        if is_aggregate(ty) {
            match &init.kind {
                ExprKind::InitList(_) | ExprKind::DesignatedInit(_) => {
                    self.lower_init_into(base, ty, init)?;
                }
                _ => {
                    let src = self.lower_aggregate_addr(init)?;
                    self.emit(IrInst::MemCpy {
                        dst: base,
                        src,
                        len: size,
                    });
                }
            }
        } else {
            let irty = scalar_ir_ty(ty).unwrap();
            let (v, vt) = self.lower_expr(init)?;
            let v = self.coerce_to_ast(v, vt, ty)?;
            self.emit(IrInst::Store {
                ty: irty,
                addr: base,
                val: v,
            });
        }
        Ok(())
    }

    /// Emit the stores for a brace/designated initializer into `addr` of type `ty`.
    fn lower_init_into(&mut self, addr: Val, ty: &Type, init: &Expr) -> Result<(), CodegenError> {
        match &init.kind {
            ExprKind::InitList(items) => match ty {
                Type::Array(elem, _) => {
                    let stride = self.layouts.stride_of(elem) as u32;
                    for (i, item) in items.iter().enumerate() {
                        let at = self.offset_addr(addr, i as u32 * stride);
                        self.lower_init_into(at, elem, item)?;
                    }
                    Ok(())
                }
                Type::Named(class) => {
                    let fields = self.aggregate_fields(class);
                    for (item, (foff, fty)) in items.iter().zip(fields) {
                        let at = self.offset_addr(addr, foff);
                        self.lower_init_into(at, &fty, item)?;
                    }
                    Ok(())
                }
                _ => Err(CodegenError::at(
                    init.span.pos,
                    "brace initializer on a scalar",
                )),
            },
            ExprKind::DesignatedInit(pairs) => {
                let Type::Named(class) = ty else {
                    return Err(CodegenError::at(
                        init.span.pos,
                        "designated initializer on a non-class",
                    ));
                };
                for (fname, fexpr) in pairs {
                    let off = self
                        .layouts
                        .offset_of(class, fname)
                        .ok_or_else(|| CodegenError::at(init.span.pos, "unknown field"))?
                        as u32;
                    let fty = self
                        .field_ty(class, fname)
                        .ok_or_else(|| CodegenError::at(init.span.pos, "unknown field"))?;
                    let at = self.offset_addr(addr, off);
                    self.lower_init_into(at, &fty, fexpr)?;
                }
                Ok(())
            }
            _ => {
                // A scalar leaf, or an aggregate copied from an lvalue.
                if is_aggregate(ty) {
                    let size = self.layouts.size_of(ty) as u32;
                    let src = self.lower_aggregate_addr(init)?;
                    self.emit(IrInst::MemCpy {
                        dst: addr,
                        src,
                        len: size,
                    });
                } else {
                    let irty = scalar_ir_ty(ty).unwrap();
                    let (v, vt) = self.lower_expr(init)?;
                    let v = self.coerce_to_ast(v, vt, ty)?;
                    self.emit(IrInst::Store {
                        ty: irty,
                        addr,
                        val: v,
                    });
                }
                Ok(())
            }
        }
    }

    fn label_block(&mut self, name: &str) -> BlockId {
        if let Some(b) = self.labels.get(name) {
            return *b;
        }
        let b = self.new_block();
        self.labels.insert(name.to_string(), b);
        b
    }

    fn lower_if(
        &mut self,
        cond: &Expr,
        then: &Stmt,
        else_: Option<&Stmt>,
    ) -> Result<(), CodegenError> {
        let c = self.lower_cond(cond)?;
        let then_b = self.new_block();
        let else_b = self.new_block();
        let join = self.new_block();
        let false_target = if else_.is_some() { else_b } else { join };
        self.terminate(IrTerm::CondBr {
            cond: c,
            t: then_b,
            f: false_target,
        });

        self.seal_block(then_b);
        self.switch_to(then_b);
        self.lower_stmt(then)?;
        if !self.cur_terminated {
            self.terminate(IrTerm::Br(join));
        }

        if let Some(else_s) = else_ {
            self.seal_block(else_b);
            self.switch_to(else_b);
            self.lower_stmt(else_s)?;
            if !self.cur_terminated {
                self.terminate(IrTerm::Br(join));
            }
        } else {
            self.seal_block(else_b);
        }

        self.seal_block(join);
        self.switch_to(join);
        Ok(())
    }

    fn lower_while(&mut self, cond: &Expr, body: &Stmt) -> Result<(), CodegenError> {
        let header = self.new_block();
        self.terminate(IrTerm::Br(header));
        self.switch_to(header);

        let c = self.lower_cond(cond)?;
        let body_b = self.new_block();
        let after = self.new_block();
        self.terminate(IrTerm::CondBr {
            cond: c,
            t: body_b,
            f: after,
        });

        self.seal_block(body_b);
        self.break_targets.push((after, self.try_depth));
        self.continue_targets.push((header, self.try_depth));
        self.switch_to(body_b);
        self.lower_stmt(body)?;
        if !self.cur_terminated {
            self.terminate(IrTerm::Br(header));
        }
        self.break_targets.pop();
        self.continue_targets.pop();

        self.seal_block(header);
        self.seal_block(after);
        self.switch_to(after);
        Ok(())
    }

    fn lower_do_while(&mut self, body: &Stmt, cond: &Expr) -> Result<(), CodegenError> {
        let body_b = self.new_block();
        self.terminate(IrTerm::Br(body_b));
        let cont = self.new_block();
        let after = self.new_block();

        self.switch_to(body_b);
        self.break_targets.push((after, self.try_depth));
        self.continue_targets.push((cont, self.try_depth));
        self.lower_stmt(body)?;
        if !self.cur_terminated {
            self.terminate(IrTerm::Br(cont));
        }
        self.break_targets.pop();
        self.continue_targets.pop();

        self.seal_block(cont);
        self.switch_to(cont);
        let c = self.lower_cond(cond)?;
        self.terminate(IrTerm::CondBr {
            cond: c,
            t: body_b,
            f: after,
        });

        self.seal_block(body_b);
        self.seal_block(after);
        self.switch_to(after);
        Ok(())
    }

    fn lower_for(
        &mut self,
        init: Option<&Stmt>,
        cond: Option<&Expr>,
        step: Option<&Expr>,
        body: &Stmt,
    ) -> Result<(), CodegenError> {
        self.scopes.push(HashMap::new());
        if let Some(init) = init {
            self.lower_stmt(init)?;
        }
        let header = self.new_block();
        self.terminate(IrTerm::Br(header));
        self.switch_to(header);

        let body_b = self.new_block();
        let step_b = self.new_block();
        let after = self.new_block();
        match cond {
            Some(cond) => {
                let c = self.lower_cond(cond)?;
                self.terminate(IrTerm::CondBr {
                    cond: c,
                    t: body_b,
                    f: after,
                });
            }
            None => self.terminate(IrTerm::Br(body_b)),
        }

        self.seal_block(body_b);
        self.break_targets.push((after, self.try_depth));
        self.continue_targets.push((step_b, self.try_depth));
        self.switch_to(body_b);
        self.lower_stmt(body)?;
        if !self.cur_terminated {
            self.terminate(IrTerm::Br(step_b));
        }
        self.break_targets.pop();
        self.continue_targets.pop();

        self.seal_block(step_b);
        self.switch_to(step_b);
        if let Some(step) = step {
            self.lower_expr_discard(step)?;
        }
        if !self.cur_terminated {
            self.terminate(IrTerm::Br(header));
        }

        self.seal_block(header);
        self.seal_block(after);
        self.switch_to(after);
        self.scopes.pop();
        Ok(())
    }

    /// Lower a `switch`. The value is evaluated once; an optional `start:` prologue
    /// runs on entry before dispatch; `case`/`default` bodies fall through (each is a
    /// block, linked by an explicit branch); `break` exits; the `end:` epilogue is
    /// reached only by fall-through. An all-constant switch lowers to an `IrTerm::Switch`
    /// (jump-table-eligible); otherwise to a compare-chain. Matching is signed `I64`,
    /// as in the interpreter.
    fn lower_switch(
        &mut self,
        cond: &Expr,
        body: &Stmt,
        pos: crate::token::Pos,
    ) -> Result<(), CodegenError> {
        let StmtKind::Block(stmts) = &body.kind else {
            return Err(CodegenError::at(pos, "switch body must be a block"));
        };

        // Evaluate the scrutinee once (before the prologue, matching the backends).
        let (sv, st) = self.lower_expr(cond)?;
        let sval = self.coerce(sv, st, IrTy::I64);

        // `start:` / `end:` partition the body into an optional prologue and epilogue.
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

        self.scopes.push(HashMap::new());
        let exit = self.new_block();

        if let Some(range) = prologue.clone() {
            for st in &stmts[range] {
                self.lower_stmt(st)?;
            }
        }

        // A block per case/default, plus the epilogue block.
        let mut block_at: HashMap<usize, BlockId> = HashMap::new();
        let mut default_block: Option<BlockId> = None;
        for (i, s) in stmts.iter().enumerate() {
            match &s.kind {
                StmtKind::Case { .. } => {
                    let b = self.new_block();
                    block_at.insert(i, b);
                }
                StmtKind::Default => {
                    let b = self.new_block();
                    block_at.insert(i, b);
                    default_block = Some(b);
                }
                _ => {}
            }
        }
        let end_block = end_idx.map(|_| self.new_block());
        let gap = default_block.or(end_block).unwrap_or(exit);

        // Dispatch: a jump table when every case bound is a constant, else a chain.
        let all_const = stmts.iter().all(|s| match &s.kind {
            StmtKind::Case { lo, hi } => {
                crate::layout::const_eval(lo).is_ok()
                    && hi
                        .as_ref()
                        .is_none_or(|h| crate::layout::const_eval(h).is_ok())
            }
            _ => true,
        });
        if all_const {
            let mut cases = Vec::new();
            for (i, s) in stmts.iter().enumerate() {
                if let StmtKind::Case { lo, hi } = &s.kind {
                    let lo = crate::layout::const_eval(lo).unwrap();
                    let hi = hi
                        .as_ref()
                        .map(|h| crate::layout::const_eval(h).unwrap())
                        .unwrap_or(lo);
                    cases.push((lo, hi, block_at[&i]));
                }
            }
            self.terminate(IrTerm::Switch {
                val: sval,
                ty: IrTy::I64,
                signed: true,
                cases,
                default: gap,
            });
        } else {
            for (i, s) in stmts.iter().enumerate() {
                if let StmtKind::Case { lo, hi } = &s.kind {
                    let target = block_at[&i];
                    let (lv, lt) = self.lower_expr(lo)?;
                    let lo_v = self.coerce(lv, lt, IrTy::I64);
                    match hi {
                        None => {
                            let next = self.new_block();
                            self.terminate(IrTerm::CondBr {
                                cond: Cond::Cmp {
                                    op: CmpOp::Eq,
                                    ty: IrTy::I64,
                                    signed: true,
                                    lhs: sval,
                                    rhs: lo_v,
                                },
                                t: target,
                                f: next,
                            });
                            self.seal_block(next);
                            self.switch_to(next);
                        }
                        Some(hi) => {
                            let lo_ok = self.new_block();
                            let next = self.new_block();
                            // v >= lo ?
                            self.terminate(IrTerm::CondBr {
                                cond: Cond::Cmp {
                                    op: CmpOp::Ge,
                                    ty: IrTy::I64,
                                    signed: true,
                                    lhs: sval,
                                    rhs: lo_v,
                                },
                                t: lo_ok,
                                f: next,
                            });
                            self.seal_block(lo_ok);
                            self.switch_to(lo_ok);
                            let (hv, ht) = self.lower_expr(hi)?;
                            let hi_v = self.coerce(hv, ht, IrTy::I64);
                            // v <= hi ?
                            self.terminate(IrTerm::CondBr {
                                cond: Cond::Cmp {
                                    op: CmpOp::Le,
                                    ty: IrTy::I64,
                                    signed: true,
                                    lhs: sval,
                                    rhs: hi_v,
                                },
                                t: target,
                                f: next,
                            });
                            self.seal_block(next);
                            self.switch_to(next);
                        }
                    }
                }
            }
            self.terminate(IrTerm::Br(gap));
        }

        // Emit the body, one block per case/default, with explicit fall-through.
        self.break_targets.push((exit, self.try_depth));
        for (i, s) in stmts.iter().enumerate() {
            if prologue.as_ref().is_some_and(|r| r.contains(&i)) {
                continue;
            }
            match &s.kind {
                StmtKind::SwitchStart => {}
                StmtKind::Case { .. } | StmtKind::Default => {
                    let b = block_at[&i];
                    if !self.cur_terminated {
                        self.terminate(IrTerm::Br(b));
                    }
                    self.switch_to(b);
                    self.seal_block(b);
                }
                StmtKind::SwitchEnd => {
                    let b = end_block.unwrap();
                    if !self.cur_terminated {
                        self.terminate(IrTerm::Br(b));
                    }
                    self.switch_to(b);
                    self.seal_block(b);
                }
                _ => self.lower_stmt(s)?,
            }
        }
        if !self.cur_terminated {
            self.terminate(IrTerm::Br(exit));
        }
        self.break_targets.pop();

        self.seal_block(exit);
        self.switch_to(exit);
        self.scopes.pop();
        Ok(())
    }

    fn lower_cond(&mut self, e: &Expr) -> Result<Cond, CodegenError> {
        if let ExprKind::Binary { op, lhs, rhs } = &e.kind {
            if let Some(cmp) = cmp_op(*op) {
                if !is_ptr_like(lhs) && !is_ptr_like(rhs) {
                    let ty = promoted(lhs, rhs);
                    let signed = signed_rel(lhs, rhs);
                    let (l, lt) = self.lower_expr(lhs)?;
                    let (r, rt) = self.lower_expr(rhs)?;
                    let l = self.coerce(l, lt, ty);
                    let r = self.coerce(r, rt, ty);
                    return Ok(Cond::Cmp {
                        op: cmp,
                        ty,
                        signed,
                        lhs: l,
                        rhs: r,
                    });
                }
            }
        }
        let (v, vt) = self.lower_expr(e)?;
        Ok(Cond::NonZero { val: v, ty: vt })
    }

    /// Lower an expression statement, honouring HolyC's implicit print: a bare string
    /// prints verbatim, and the `"fmt", args` comma form formats through `Print`. (A
    /// comma in any other position is the sequencing operator — see [`Self::lower_expr_discard`].)
    fn lower_stmt_expr(&mut self, e: &Expr) -> Result<(), CodegenError> {
        match &e.kind {
            ExprKind::Str(s) => {
                self.emit_string_print(s);
                Ok(())
            }
            ExprKind::Comma(items) => self
                .lower_named_call("Print", items, e.span.pos)
                .map(|_| ()),
            _ => self.lower_expr_discard(e),
        }
    }

    /// Evaluate an expression for its side effects only (a `for` step, a discarded
    /// result). A comma here is the sequencing operator, not a print.
    fn lower_expr_discard(&mut self, e: &Expr) -> Result<(), CodegenError> {
        self.lower_expr(e).map(|_| ())
    }

    /// Lower a bare string statement to a direct `StdWrite(1, &str, len)`.
    fn emit_string_print(&mut self, s: &str) {
        let len = s.len() as i64;
        let id = self.strings.intern(s);
        let addr = self.fresh_vreg();
        self.emit(IrInst::StrAddr { dst: addr, str: id });
        self.emit(IrInst::Prim {
            dst: None,
            prim: Prim::StdWrite,
            args: vec![Val::ImmInt(1), Val::Reg(addr), Val::ImmInt(len)],
            width: None,
        });
    }

    // ---- expressions ----

    fn lower_expr(&mut self, e: &Expr) -> Result<(Val, IrTy), CodegenError> {
        let pos = e.span.pos;
        match &e.kind {
            ExprKind::Int(v) | ExprKind::Char(v) => Ok((Val::ImmInt(*v), self.expr_ty(e))),
            ExprKind::Float(f) => Ok((Val::ImmF64(f.to_bits()), IrTy::F64)),
            ExprKind::Str(s) => {
                // A string literal in value position is a pointer to its interned,
                // NUL-terminated bytes (one stable address per distinct literal).
                let id = self.strings.intern(s);
                let dst = self.fresh_vreg();
                self.emit(IrInst::StrAddr { dst, str: id });
                Ok((Val::Reg(dst), IrTy::Ptr))
            }
            ExprKind::Ident(_)
            | ExprKind::Index { .. }
            | ExprKind::Member { .. }
            | ExprKind::Unary {
                op: UnOp::Deref, ..
            } => self.lower_lvalue_rvalue(e),
            ExprKind::Unary { op, expr } => self.lower_unary(*op, expr, e),
            ExprKind::Postfix { op, expr } => self.lower_postfix(*op, expr),
            ExprKind::Binary { op, lhs, rhs } => self.lower_binary(*op, lhs, rhs, e),
            ExprKind::Assign { op, target, value } => self.lower_assign(*op, target, value),
            ExprKind::Ternary { cond, then, else_ } => self.lower_ternary(cond, then, else_, e),
            ExprKind::Call { callee, args } => self.lower_call(callee, args, e),
            ExprKind::Cast { ty, expr } => {
                let (v, vt) = self.lower_expr(expr)?;
                let (v, to) = self.coerce_to_ast_typed(v, vt, ty)?;
                Ok((v, to))
            }
            ExprKind::Sizeof(arg) => {
                let sz = match arg {
                    SizeofArg::Type(t) => self.layouts.size_of(t),
                    SizeofArg::Expr(inner) => {
                        let t = inner
                            .ty()
                            .ok_or_else(|| CodegenError::at(pos, "sizeof of untyped expression"))?;
                        self.layouts.size_of(&t)
                    }
                };
                Ok((Val::ImmInt(sz as i64), IrTy::I64))
            }
            ExprKind::Offset { class, path } => {
                let off = self
                    .layouts
                    .nested_offset_of(class, path)
                    .ok_or_else(|| CodegenError::at(pos, "offset of unknown member"))?;
                Ok((Val::ImmInt(off as i64), IrTy::I64))
            }
            ExprKind::Comma(es) => {
                let mut last = (Val::ImmInt(0), IrTy::I64);
                for sub in es {
                    last = self.lower_expr(sub)?;
                }
                Ok(last)
            }
            _ => Err(CodegenError::at(pos, "expression not yet lowered")),
        }
    }

    /// Lower an lvalue expression used as an rvalue: scalar → `Load`; array → decayed
    /// address; class/union rvalue is unsupported here (handled by the aggregate path).
    fn lower_lvalue_rvalue(&mut self, e: &Expr) -> Result<(Val, IrTy), CodegenError> {
        if let ExprKind::Ident(name) = &e.kind {
            match self.lookup(name) {
                // An SSA-resident scalar identifier reads straight from its value. An
                // array parameter (SSA pointer with an aggregate type) instead decays
                // via `lower_lvalue` below.
                Some(info) => {
                    if let Place::Ssa(id) = info.place {
                        if !is_aggregate(&info.ty) {
                            let ty = scalar_ir_ty(&info.ty).unwrap_or(IrTy::I64);
                            let cur = self.cur;
                            return Ok((self.read_variable(id, cur), ty));
                        }
                    }
                }
                // A bare function name is a zero-argument call (HolyC's `Main;`).
                None if self.sigs.contains_key(name) => {
                    return self.lower_named_call(name, &[], e.span.pos);
                }
                None => {}
            }
        }
        let lv = self.lower_lvalue(e)?;
        let ast = lvalue_ast(&lv).clone();
        match &ast {
            Type::Array(..) => {
                // Array decays to a pointer to its storage.
                Ok((
                    lvalue_addr(&lv).expect("array lvalue has an address"),
                    IrTy::Ptr,
                ))
            }
            Type::Named(_) => Err(CodegenError::at(
                e.span.pos,
                "aggregate value in scalar context not yet lowered",
            )),
            _ => self.load_lvalue(&lv),
        }
    }

    fn lower_unary(
        &mut self,
        op: UnOp,
        expr: &Expr,
        whole: &Expr,
    ) -> Result<(Val, IrTy), CodegenError> {
        match op {
            UnOp::Pos => self.lower_expr(expr),
            UnOp::Neg => {
                let (v, vt) = self.lower_expr(expr)?;
                let ty = if vt.is_float() { IrTy::F64 } else { IrTy::I64 };
                let v = self.coerce(v, vt, ty);
                let dst = self.fresh_vreg();
                self.emit(IrInst::Un {
                    dst,
                    op: IrUnOp::Neg,
                    ty,
                    src: v,
                });
                Ok((Val::Reg(dst), ty))
            }
            UnOp::BitNot => {
                let (v, vt) = self.lower_expr(expr)?;
                let v = self.coerce(v, vt, IrTy::I64);
                let dst = self.fresh_vreg();
                self.emit(IrInst::Un {
                    dst,
                    op: IrUnOp::BitNot,
                    ty: IrTy::I64,
                    src: v,
                });
                Ok((Val::Reg(dst), IrTy::I64))
            }
            UnOp::Not => {
                let (v, vt) = self.lower_expr(expr)?;
                let dst = self.fresh_vreg();
                let zero = if vt.is_float() {
                    Val::ImmF64(0)
                } else {
                    Val::ImmInt(0)
                };
                self.emit(IrInst::Cmp {
                    dst,
                    op: CmpOp::Eq,
                    ty: vt,
                    signed: false,
                    lhs: v,
                    rhs: zero,
                });
                Ok((Val::Reg(dst), IrTy::I64))
            }
            UnOp::AddrOf => {
                // `&Func` is a self-resolved function address.
                if let ExprKind::Ident(name) = &expr.kind {
                    if self.lookup(name).is_none() && self.sigs.contains_key(name) {
                        let dst = self.fresh_vreg();
                        self.emit(IrInst::FuncAddr {
                            dst,
                            func: name.clone(),
                        });
                        return Ok((Val::Reg(dst), IrTy::Ptr));
                    }
                }
                let lv = self.lower_lvalue(expr)?;
                let addr = lvalue_addr(&lv)
                    .ok_or_else(|| CodegenError::at(whole.span.pos, "cannot take address"))?;
                Ok((addr, IrTy::Ptr))
            }
            UnOp::PreInc | UnOp::PreDec => {
                let lv = self.lower_lvalue(expr)?;
                let (old, ty) = self.load_lvalue(&lv)?;
                let new = self.inc_dec(old, ty, op == UnOp::PreInc, &lv);
                self.store_lvalue(&lv, new);
                Ok((new, ty))
            }
            UnOp::Deref => unreachable!("deref handled by lower_lvalue_rvalue"),
        }
    }

    fn lower_postfix(&mut self, op: PostOp, expr: &Expr) -> Result<(Val, IrTy), CodegenError> {
        let lv = self.lower_lvalue(expr)?;
        let (old, ty) = self.load_lvalue(&lv)?;
        let new = self.inc_dec(old, ty, op == PostOp::Inc, &lv);
        self.store_lvalue(&lv, new);
        Ok((old, ty))
    }

    /// `old ± 1`, scaled by the pointee size for pointers, coerced back to `ty`.
    fn inc_dec(&mut self, old: Val, ty: IrTy, inc: bool, lv: &LValue) -> Val {
        if ty == IrTy::Ptr {
            let stride = deref_ty(lvalue_ast(lv))
                .map(|e| self.layouts.stride_of(e) as i64)
                .unwrap_or(1);
            let step = if inc { stride } else { -stride };
            let dst = self.fresh_vreg();
            self.emit(IrInst::PtrAdd {
                dst,
                base: old,
                index: Val::ImmInt(step),
                stride: 1,
            });
            return Val::Reg(dst);
        }
        let pty = if ty.is_float() { IrTy::F64 } else { IrTy::I64 };
        let one = if pty.is_float() {
            Val::ImmF64(1.0f64.to_bits())
        } else {
            Val::ImmInt(1)
        };
        let dst = self.fresh_vreg();
        self.emit(IrInst::Bin {
            dst,
            op: if inc { IrBinOp::Add } else { IrBinOp::Sub },
            ty: pty,
            signed: true,
            lhs: old,
            rhs: one,
        });
        self.coerce(Val::Reg(dst), pty, ty)
    }

    fn lower_binary(
        &mut self,
        op: BinOp,
        lhs: &Expr,
        rhs: &Expr,
        whole: &Expr,
    ) -> Result<(Val, IrTy), CodegenError> {
        if op == BinOp::And || op == BinOp::Or {
            return self.lower_logical(op, lhs, rhs);
        }
        // Pointer arithmetic and comparison.
        let lptr = is_ptr_like(lhs);
        let rptr = is_ptr_like(rhs);
        if (lptr || rptr) && matches!(op, BinOp::Add | BinOp::Sub) {
            return self.lower_ptr_arith(op, lhs, rhs, lptr, rptr);
        }
        if let Some(cmp) = cmp_op(op) {
            if lptr || rptr {
                let l = self.lower_ptr_value(lhs)?;
                let r = self.lower_ptr_value(rhs)?;
                let dst = self.fresh_vreg();
                self.emit(IrInst::Cmp {
                    dst,
                    op: cmp,
                    ty: IrTy::Ptr,
                    signed: false,
                    lhs: l,
                    rhs: r,
                });
                return Ok((Val::Reg(dst), IrTy::I64));
            }
            let ty = promoted(lhs, rhs);
            let signed = signed_rel(lhs, rhs);
            let (l, lt) = self.lower_expr(lhs)?;
            let (r, rt) = self.lower_expr(rhs)?;
            let l = self.coerce(l, lt, ty);
            let r = self.coerce(r, rt, ty);
            let dst = self.fresh_vreg();
            self.emit(IrInst::Cmp {
                dst,
                op: cmp,
                ty,
                signed,
                lhs: l,
                rhs: r,
            });
            return Ok((Val::Reg(dst), IrTy::I64));
        }
        let ty = promoted(lhs, rhs);
        let signed = signed_left(lhs);
        let irop = arith_op(op)
            .ok_or_else(|| CodegenError::at(whole.span.pos, "binary operator not yet lowered"))?;
        let (l, lt) = self.lower_expr(lhs)?;
        let (r, rt) = self.lower_expr(rhs)?;
        let l = self.coerce(l, lt, ty);
        let r = self.coerce(r, rt, ty);
        let dst = self.fresh_vreg();
        self.emit(IrInst::Bin {
            dst,
            op: irop,
            ty,
            signed,
            lhs: l,
            rhs: r,
        });
        Ok((Val::Reg(dst), ty))
    }

    /// Pointer ± integer (scaled), and pointer − pointer (element count).
    fn lower_ptr_arith(
        &mut self,
        op: BinOp,
        lhs: &Expr,
        rhs: &Expr,
        lptr: bool,
        rptr: bool,
    ) -> Result<(Val, IrTy), CodegenError> {
        if op == BinOp::Sub && lptr && rptr {
            let elem = deref_ty(&lhs.ty().unwrap()).cloned();
            let stride = elem.map(|e| self.layouts.stride_of(&e) as i64).unwrap_or(1);
            let a = self.lower_ptr_value(lhs)?;
            let b = self.lower_ptr_value(rhs)?;
            let diff = self.fresh_vreg();
            self.emit(IrInst::Bin {
                dst: diff,
                op: IrBinOp::Sub,
                ty: IrTy::I64,
                signed: true,
                lhs: a,
                rhs: b,
            });
            let res = self.fresh_vreg();
            self.emit(IrInst::Bin {
                dst: res,
                op: IrBinOp::Div,
                ty: IrTy::I64,
                signed: true,
                lhs: Val::Reg(diff),
                rhs: Val::ImmInt(stride.max(1)),
            });
            return Ok((Val::Reg(res), IrTy::I64));
        }
        // pointer ± integer.
        let (ptr_e, int_e) = if lptr { (lhs, rhs) } else { (rhs, lhs) };
        let stride = deref_ty(&ptr_e.ty().unwrap())
            .map(|e| self.layouts.stride_of(e) as u32)
            .unwrap_or(1);
        let p = self.lower_ptr_value(ptr_e)?;
        let (i, it) = self.lower_expr(int_e)?;
        let mut i = self.coerce(i, it, IrTy::I64);
        if op == BinOp::Sub {
            let neg = self.fresh_vreg();
            self.emit(IrInst::Un {
                dst: neg,
                op: IrUnOp::Neg,
                ty: IrTy::I64,
                src: i,
            });
            i = Val::Reg(neg);
        }
        let dst = self.fresh_vreg();
        self.emit(IrInst::PtrAdd {
            dst,
            base: p,
            index: i,
            stride,
        });
        Ok((Val::Reg(dst), IrTy::Ptr))
    }

    /// Lower an operand to a pointer value (an address). Arrays decay; `NULL`/`0`
    /// becomes the null address.
    fn lower_ptr_value(&mut self, e: &Expr) -> Result<Val, CodegenError> {
        let (v, vt) = self.lower_expr(e)?;
        Ok(self.coerce(v, vt, IrTy::Ptr))
    }

    fn lower_logical(
        &mut self,
        op: BinOp,
        lhs: &Expr,
        rhs: &Expr,
    ) -> Result<(Val, IrTy), CodegenError> {
        let res = self.alloc_var(IrTy::I64);
        let rhs_b = self.new_block();
        let short_b = self.new_block();
        let join = self.new_block();

        let c = self.lower_cond(lhs)?;
        let (t, f) = if op == BinOp::And {
            (rhs_b, short_b)
        } else {
            (short_b, rhs_b)
        };
        self.terminate(IrTerm::CondBr { cond: c, t, f });

        self.seal_block(short_b);
        self.switch_to(short_b);
        let short_val = if op == BinOp::And {
            Val::ImmInt(0)
        } else {
            Val::ImmInt(1)
        };
        self.write_variable(res, short_b, short_val);
        self.terminate(IrTerm::Br(join));

        self.seal_block(rhs_b);
        self.switch_to(rhs_b);
        let (rv, rt) = self.lower_expr(rhs)?;
        let norm = self.fresh_vreg();
        let zero = if rt.is_float() {
            Val::ImmF64(0)
        } else {
            Val::ImmInt(0)
        };
        self.emit(IrInst::Cmp {
            dst: norm,
            op: CmpOp::Ne,
            ty: rt,
            signed: false,
            lhs: rv,
            rhs: zero,
        });
        let cur = self.cur;
        self.write_variable(res, cur, Val::Reg(norm));
        self.terminate(IrTerm::Br(join));

        self.seal_block(join);
        self.switch_to(join);
        let v = self.read_variable(res, join);
        Ok((v, IrTy::I64))
    }

    fn lower_assign(
        &mut self,
        op: AssignOp,
        target: &Expr,
        value: &Expr,
    ) -> Result<(Val, IrTy), CodegenError> {
        // Aggregate assignment is a by-value copy.
        if matches!(target.ty(), Some(t) if is_aggregate(&t)) {
            if op != AssignOp::Assign {
                return Err(CodegenError::at(
                    target.span.pos,
                    "compound assignment on an aggregate",
                ));
            }
            let tty = target.ty().unwrap();
            let dst = self.lower_aggregate_addr(target)?;
            let src = self.lower_aggregate_addr(value)?;
            let len = self.layouts.size_of(&tty) as u32;
            self.emit(IrInst::MemCpy { dst, src, len });
            return Ok((dst, IrTy::Ptr));
        }

        let lv = self.lower_lvalue(target)?;
        let ast = lvalue_ast(&lv).clone();
        if op == AssignOp::Assign {
            let (v, vt) = self.lower_expr(value)?;
            let v = self.coerce_to_ast(v, vt, &ast)?;
            self.store_lvalue(&lv, v);
            return Ok((v, scalar_ir_ty(&ast).unwrap_or(IrTy::I64)));
        }

        // Compound assignment. Pointers use scaled add/sub; scalars combine at the
        // promoted width then truncate.
        let target_irty = scalar_ir_ty(&ast).unwrap_or(IrTy::I64);
        if target_irty == IrTy::Ptr && matches!(op, AssignOp::Add | AssignOp::Sub) {
            let (old, _) = self.load_lvalue(&lv)?;
            let stride = deref_ty(&ast)
                .map(|e| self.layouts.stride_of(e) as u32)
                .unwrap_or(1);
            let (i, it) = self.lower_expr(value)?;
            let mut i = self.coerce(i, it, IrTy::I64);
            if op == AssignOp::Sub {
                let neg = self.fresh_vreg();
                self.emit(IrInst::Un {
                    dst: neg,
                    op: IrUnOp::Neg,
                    ty: IrTy::I64,
                    src: i,
                });
                i = Val::Reg(neg);
            }
            let dst = self.fresh_vreg();
            self.emit(IrInst::PtrAdd {
                dst,
                base: old,
                index: i,
                stride,
            });
            self.store_lvalue(&lv, Val::Reg(dst));
            return Ok((Val::Reg(dst), IrTy::Ptr));
        }

        let pty = if target_irty.is_float() || matches!(value.ty(), Some(Type::F64)) {
            IrTy::F64
        } else {
            IrTy::I64
        };
        let (old, ot) = self.load_lvalue(&lv)?;
        let old = self.coerce(old, ot, pty);
        let (rv, rt) = self.lower_expr(value)?;
        let rv = self.coerce(rv, rt, pty);
        let irop = arith_op(compound_binop(op)).expect("compound op is arithmetic");
        let dst = self.fresh_vreg();
        self.emit(IrInst::Bin {
            dst,
            op: irop,
            ty: pty,
            signed: signed_left(target),
            lhs: old,
            rhs: rv,
        });
        let res = self.coerce_to_ast(Val::Reg(dst), pty, &ast)?;
        self.store_lvalue(&lv, res);
        Ok((res, target_irty))
    }

    fn lower_ternary(
        &mut self,
        cond: &Expr,
        then: &Expr,
        else_: &Expr,
        whole: &Expr,
    ) -> Result<(Val, IrTy), CodegenError> {
        let ty = self.expr_ty(whole);
        let res = self.alloc_var(ty);
        let then_b = self.new_block();
        let else_b = self.new_block();
        let join = self.new_block();
        let c = self.lower_cond(cond)?;
        self.terminate(IrTerm::CondBr {
            cond: c,
            t: then_b,
            f: else_b,
        });

        self.seal_block(then_b);
        self.switch_to(then_b);
        let (tv, tt) = self.lower_expr(then)?;
        let tv = self.coerce(tv, tt, ty);
        let cur = self.cur;
        self.write_variable(res, cur, tv);
        self.terminate(IrTerm::Br(join));

        self.seal_block(else_b);
        self.switch_to(else_b);
        let (ev, et) = self.lower_expr(else_)?;
        let ev = self.coerce(ev, et, ty);
        let cur = self.cur;
        self.write_variable(res, cur, ev);
        self.terminate(IrTerm::Br(join));

        self.seal_block(join);
        self.switch_to(join);
        let v = self.read_variable(res, join);
        Ok((v, ty))
    }

    fn lower_call(
        &mut self,
        callee: &Expr,
        args: &[Expr],
        whole: &Expr,
    ) -> Result<(Val, IrTy), CodegenError> {
        let pos = whole.span.pos;
        // A name bound to a local **or a global variable** (a function-pointer variable)
        // is an indirect call through that variable's value. Any other bare name is a
        // direct/named call (a function, or `lower_named_call`'s primitive/bare handling).
        if let ExprKind::Ident(name) = &callee.kind {
            if self.lookup(name).is_none() && !self.globals.contains_key(name) {
                return self.lower_named_call(name, args, pos);
            }
        }
        self.lower_indirect_call(callee, args, pos)
    }

    /// Lower a call through a function-pointer value (a variable, field, or `(*fp)`).
    /// The signature comes from the callee's `FuncPtr` type.
    fn lower_indirect_call(
        &mut self,
        callee: &Expr,
        args: &[Expr],
        pos: crate::token::Pos,
    ) -> Result<(Val, IrTy), CodegenError> {
        let (ret_ty, params) = match callee.ty() {
            Some(Type::FuncPtr { ret, params }) => (*ret, params),
            _ => {
                return Err(CodegenError::at(
                    pos,
                    "indirect call on a non-function-pointer",
                ));
            }
        };
        let ret = ret_of(&ret_ty, self.layouts);
        // Evaluate the callee (the function address) before the arguments.
        let callee_val = self.lower_ptr_value(callee)?;
        let mut ir_args = Vec::with_capacity(args.len());
        for (i, a) in args.iter().enumerate() {
            if i < params.len() {
                let arg = self.lower_fixed_arg(a, &params[i], pos)?;
                ir_args.push(arg);
            } else {
                let (v, vt) = self.lower_expr(a)?;
                ir_args.push(ArgVal {
                    ty: if vt.is_float() {
                        ArgTy::Float
                    } else {
                        ArgTy::Int(vt)
                    },
                    val: v,
                });
            }
        }
        let sret = self.alloc_sret(ret);
        let dst = if matches!(ret, IrRet::Void | IrRet::Agg { .. }) {
            None
        } else {
            Some(self.fresh_vreg())
        };
        self.emit(IrInst::Call {
            dst,
            ret,
            callee: Callee::Indirect(callee_val),
            args: ir_args,
            sret,
            varargs: VarargInfo::default(),
        });
        Ok(self.call_result(ret, dst, sret))
    }

    /// Lower a direct call by name (also the entry point for the `"fmt", args` print
    /// form, which desugars to `Print(...)`). Variadic callees receive their fixed
    /// args plus two hidden trailing args: `VargC` (the variadic count) and `VargV`
    /// (a pointer to a packed 8-byte-per-arg buffer).
    fn lower_named_call(
        &mut self,
        name: &str,
        args: &[Expr],
        pos: crate::token::Pos,
    ) -> Result<(Val, IrTy), CodegenError> {
        let is_prim = !self.defined.contains(name) && crate::intrinsics::is_primitive(name);
        let sig = self.sigs.get(name);
        let ret = sig
            .map(|s| ret_of(&s.ret, self.layouts))
            .unwrap_or(IrRet::Void);

        if is_prim {
            let prim = Prim::from_name(name).ok_or_else(|| {
                CodegenError::at(pos, format!("primitive {name} not yet lowered"))
            })?;
            // Atomic ops are width-directed by the pointee of their first (pointer)
            // argument; the backend uses it for both the access size and the
            // sign/zero-extension of the result.
            let width = if matches!(
                prim,
                Prim::AtomicLoad
                    | Prim::AtomicStore
                    | Prim::AtomicAdd
                    | Prim::AtomicSwap
                    | Prim::AtomicCas
            ) {
                args.first()
                    .and_then(|a| a.ty())
                    .as_ref()
                    .and_then(deref_ty)
                    .and_then(scalar_ir_ty)
                    .or(Some(IrTy::I64))
            } else {
                None
            };
            let mut vals = Vec::with_capacity(args.len());
            for a in args {
                let (v, _) = self.lower_expr(a)?;
                vals.push(v);
            }
            let dst = if matches!(ret, IrRet::Void) {
                None
            } else {
                Some(self.fresh_vreg())
            };
            self.emit(IrInst::Prim {
                dst,
                prim,
                args: vals,
                width,
            });
            return Ok((dst.map(Val::Reg).unwrap_or(Val::ImmInt(0)), ret_scalar(ret)));
        }

        let is_varargs = sig.map(|s| s.varargs).unwrap_or(false);
        let params: Vec<Type> = sig.map(|s| s.params.clone()).unwrap_or_default();
        let param_names: Vec<Option<String>> =
            sig.map(|s| s.param_names.clone()).unwrap_or_default();
        let defaults: Vec<Option<Expr>> = sig.map(|s| s.defaults.clone()).unwrap_or_default();
        let fixed = params.len().min(args.len());
        let mut ir_args = Vec::with_capacity(params.len());

        // Fixed (named) arguments, coerced to their parameter type, in the caller's scope.
        for (i, a) in args.iter().enumerate().take(fixed) {
            let arg = self.lower_fixed_arg(a, &params[i], pos)?;
            ir_args.push(arg);
        }
        // Omitted trailing parameters take their default value. A default is evaluated in
        // the **callee's** parameter scope (the earlier parameters bound to the actual
        // argument values, plus globals — never the caller's locals), so `b = a + 1`
        // resolves `a` to the argument, and `b = g` resolves `g` to the global even when
        // the caller has a same-named local. This matches the interpreter.
        if args.len() < params.len() {
            let saved = std::mem::replace(&mut self.scopes, vec![HashMap::new()]);
            for i in 0..fixed {
                let name = param_names.get(i).and_then(|n| n.as_ref());
                self.bind_param_value(name, &params[i], &ir_args[i]);
            }
            for i in args.len()..params.len() {
                let def = defaults[i]
                    .clone()
                    .ok_or_else(|| CodegenError::at(pos, "missing argument with no default"))?;
                let arg = self.lower_fixed_arg(&def, &params[i], pos)?;
                let name = param_names.get(i).and_then(|n| n.as_ref());
                self.bind_param_value(name, &params[i], &arg);
                ir_args.push(arg);
            }
            self.scopes = saved;
        }

        let mut vbuf = None;
        if is_varargs {
            // Pack the variadic arguments into a frame buffer, 8 bytes each: a float by
            // its bit pattern, a pointer by its address, everything else widened to I64.
            let var_args = &args[fixed..];
            let nvar = var_args.len() as u32;
            let slot = self.add_slot((8 * nvar).max(8), 8, SlotKind::VarargBuf, None);
            let buf = self.slot_addr(slot, 0);
            for (k, a) in var_args.iter().enumerate() {
                let (v, vt) = self.lower_expr(a)?;
                let at = self.offset_addr(buf, k as u32 * 8);
                let (sty, sval) = if vt.is_float() {
                    (IrTy::F64, v)
                } else if vt == IrTy::Ptr {
                    (IrTy::Ptr, v)
                } else {
                    (IrTy::I64, self.coerce(v, vt, IrTy::I64))
                };
                self.emit(IrInst::Store {
                    ty: sty,
                    addr: at,
                    val: sval,
                });
            }
            // The hidden `VargC` (count) and `VargV` (buffer) trailing arguments.
            ir_args.push(ArgVal {
                ty: ArgTy::Int(IrTy::I64),
                val: Val::ImmInt(nvar as i64),
            });
            ir_args.push(ArgVal {
                ty: ArgTy::Int(IrTy::Ptr),
                val: buf,
            });
            vbuf = Some((slot, 0));
        } else if args.len() > fixed {
            return Err(CodegenError::at(pos, "too many arguments"));
        }

        let sret = self.alloc_sret(ret);
        let dst = if matches!(ret, IrRet::Void | IrRet::Agg { .. }) {
            None
        } else {
            Some(self.fresh_vreg())
        };
        self.emit(IrInst::Call {
            dst,
            ret,
            callee: Callee::Direct(name.to_string()),
            args: ir_args,
            sret,
            varargs: VarargInfo {
                is_varargs,
                buf: vbuf,
                count: (args.len() - params.len().min(args.len())) as u32,
            },
        });
        Ok(self.call_result(ret, dst, sret))
    }

    /// The `(value, type)` a call expression yields: an aggregate return is its sret
    /// slot address; a scalar return is its result register; void is a placeholder.
    fn call_result(&self, ret: IrRet, dst: Option<Vreg>, sret: Option<Val>) -> (Val, IrTy) {
        match ret {
            IrRet::Agg { .. } => (sret.unwrap_or(Val::ImmInt(0)), IrTy::Ptr),
            _ => (dst.map(Val::Reg).unwrap_or(Val::ImmInt(0)), ret_scalar(ret)),
        }
    }

    // ---- lvalues ----

    fn lower_lvalue(&mut self, e: &Expr) -> Result<LValue, CodegenError> {
        match &e.kind {
            ExprKind::Ident(name) => {
                if let Some(info) = self.lookup(name) {
                    let ast = info.ty.clone();
                    return match info.place {
                        // An array parameter is an SSA register holding the pointer to
                        // its data; the "lvalue" is the memory at that pointer.
                        Place::Ssa(id) if is_aggregate(&ast) => {
                            let cur = self.cur;
                            let addr = self.read_variable(id, cur);
                            Ok(LValue::Mem { addr, ast })
                        }
                        Place::Ssa(id) => Ok(LValue::Ssa { id, ast }),
                        Place::Mem(slot) => {
                            let addr = self.slot_addr(slot, 0);
                            Ok(LValue::Mem { addr, ast })
                        }
                    };
                }
                // A reference to a global variable.
                if let Some((gid, ast)) = self.globals.get(name).map(|(g, t)| (*g, t.clone())) {
                    let addr = self.global_addr(gid, 0);
                    return Ok(LValue::Mem { addr, ast });
                }
                Err(CodegenError::at(e.span.pos, "unknown identifier"))
            }
            ExprKind::Unary {
                op: UnOp::Deref,
                expr,
            } => {
                let pointee = deref_ty(&expr.ty().unwrap_or(Type::I64))
                    .cloned()
                    .ok_or_else(|| CodegenError::at(e.span.pos, "dereference of non-pointer"))?;
                let addr = self.lower_ptr_value(expr)?;
                Ok(LValue::Mem { addr, ast: pointee })
            }
            ExprKind::Index { base, index } => {
                if let Some(member) = crate::ast::tuple_index_as_member(e) {
                    return self.lower_lvalue(&member);
                }
                let bty = base.ty().unwrap_or(Type::I64);
                let elem = deref_ty(&bty)
                    .cloned()
                    .ok_or_else(|| CodegenError::at(e.span.pos, "indexing a non-array/pointer"))?;
                let base_addr = self.array_or_ptr_base(base)?;
                let (idx, it) = self.lower_expr(index)?;
                let idx = self.coerce(idx, it, IrTy::I64);
                let stride = self.layouts.stride_of(&elem) as u32;
                let dst = self.fresh_vreg();
                self.emit(IrInst::PtrAdd {
                    dst,
                    base: base_addr,
                    index: idx,
                    stride,
                });
                Ok(LValue::Mem {
                    addr: Val::Reg(dst),
                    ast: elem,
                })
            }
            ExprKind::Member { base, field, arrow } => {
                let (base_addr, class) = if *arrow {
                    let bty = base.ty().unwrap_or(Type::I64);
                    let inner = deref_ty(&bty)
                        .cloned()
                        .ok_or_else(|| CodegenError::at(e.span.pos, "-> on a non-pointer"))?;
                    let addr = self.lower_ptr_value(base)?;
                    (addr, class_name(&inner)?)
                } else {
                    // `.` on an aggregate lvalue, or on a call that returns one by value.
                    let bty = base.ty().unwrap_or(Type::I64);
                    let addr = self.lower_aggregate_addr(base)?;
                    (addr, class_name(&bty)?)
                };
                let off = self
                    .layouts
                    .offset_of(&class, field)
                    .ok_or_else(|| CodegenError::at(e.span.pos, "unknown field"))?
                    as u32;
                let fty = self
                    .field_ty(&class, field)
                    .ok_or_else(|| CodegenError::at(e.span.pos, "unknown field"))?;
                let addr = self.offset_addr(base_addr, off);
                Ok(LValue::Mem { addr, ast: fty })
            }
            _ => Err(CodegenError::at(e.span.pos, "expression is not an lvalue")),
        }
    }

    /// The base address for indexing: an array's storage address, or a pointer's value.
    fn array_or_ptr_base(&mut self, base: &Expr) -> Result<Val, CodegenError> {
        match base.ty() {
            Some(Type::Array(..)) => {
                let lv = self.lower_lvalue(base)?;
                lvalue_addr(&lv)
                    .ok_or_else(|| CodegenError::at(base.span.pos, "array has no address"))
            }
            _ => self.lower_ptr_value(base),
        }
    }

    /// The address of an aggregate value: an lvalue's storage, or, for a call that
    /// returns an aggregate by value, its sret result slot (which `lower_call` returns
    /// as the value).
    fn lower_aggregate_addr(&mut self, e: &Expr) -> Result<Val, CodegenError> {
        match &e.kind {
            // A call's aggregate result is its sret slot (returned as the value).
            ExprKind::Call { .. } => Ok(self.lower_expr(e)?.0),
            // A tuple/aggregate literal materialises into a fresh temp slot.
            ExprKind::InitList(_) | ExprKind::DesignatedInit(_) => {
                let ty = e
                    .ty()
                    .ok_or_else(|| CodegenError::at(e.span.pos, "untyped aggregate literal"))?;
                let size = self.layouts.size_of(&ty) as u32;
                let align = self.layouts.align_of(&ty) as u32;
                let slot = self.add_slot(size, align, SlotKind::Temp, None);
                let addr = self.slot_addr(slot, 0);
                self.emit(IrInst::MemZero {
                    dst: addr,
                    len: size,
                });
                self.lower_init_into(addr, &ty, e)?;
                Ok(addr)
            }
            _ => {
                let lv = self.lower_lvalue(e)?;
                lvalue_addr(&lv)
                    .ok_or_else(|| CodegenError::at(e.span.pos, "aggregate value has no address"))
            }
        }
    }

    /// Allocate an sret result slot for an aggregate-returning call, returning its
    /// address; `None` for scalar/void returns.
    fn alloc_sret(&mut self, ret: IrRet) -> Option<Val> {
        if let IrRet::Agg { size, align } = ret {
            let slot = self.add_slot(size, align, SlotKind::Sret, None);
            Some(self.slot_addr(slot, 0))
        } else {
            None
        }
    }

    /// Lower one fixed (declared) call argument, coercing to its parameter type. An
    /// aggregate is passed by address (`AggAddr`); the callee copies it.
    /// Bind a parameter `name` to its already-lowered argument value in the current
    /// scope, so a later default expression in the same call can reference it. Scalar
    /// (int/float) parameters only — a default referencing an aggregate parameter is not
    /// supported (and is not produced by any real code).
    fn bind_param_value(&mut self, name: Option<&String>, ty: &Type, arg: &ArgVal) {
        let Some(name) = name else { return };
        let val = match arg.ty {
            ArgTy::Int(_) | ArgTy::Float => arg.val,
            ArgTy::AggAddr { .. } => return,
        };
        let id = self.bind_ssa(name, ty.clone());
        let cur = self.cur;
        self.write_variable(id, cur, val);
    }

    fn lower_fixed_arg(
        &mut self,
        a: &Expr,
        pty: &Type,
        pos: crate::token::Pos,
    ) -> Result<ArgVal, CodegenError> {
        // An array parameter decays to a pointer (by reference).
        if matches!(pty, Type::Array(..)) {
            let addr = self.lower_ptr_value(a)?;
            return Ok(ArgVal {
                ty: ArgTy::Int(IrTy::Ptr),
                val: addr,
            });
        }
        // A class/union parameter is passed by value, carried by address.
        if is_aggregate(pty) {
            let size = self.layouts.size_of(pty) as u32;
            let align = self.layouts.align_of(pty) as u32;
            let addr = self.lower_aggregate_addr(a)?;
            return Ok(ArgVal {
                ty: ArgTy::AggAddr { size, align },
                val: addr,
            });
        }
        let (v, vt) = self.lower_expr(a)?;
        let pity = scalar_ir_ty(pty)
            .ok_or_else(|| CodegenError::at(pos, "non-scalar argument not lowered"))?;
        let v = self.coerce(v, vt, pity);
        Ok(ArgVal {
            ty: if pity.is_float() {
                ArgTy::Float
            } else {
                ArgTy::Int(pity)
            },
            val: v,
        })
    }

    fn load_lvalue(&mut self, lv: &LValue) -> Result<(Val, IrTy), CodegenError> {
        match lv {
            LValue::Ssa { id, ast } => {
                let ty = scalar_ir_ty(ast).unwrap_or(IrTy::I64);
                let cur = self.cur;
                Ok((self.read_variable(*id, cur), ty))
            }
            LValue::Mem { addr, ast } => {
                let ty = scalar_ir_ty(ast)
                    .ok_or_else(|| CodegenError::new("load of an aggregate lvalue", None))?;
                let dst = self.fresh_vreg();
                self.emit(IrInst::Load {
                    dst,
                    ty,
                    addr: *addr,
                });
                Ok((Val::Reg(dst), ty))
            }
        }
    }

    /// Store an already-coerced scalar value into an lvalue.
    fn store_lvalue(&mut self, lv: &LValue, val: Val) {
        match lv {
            LValue::Ssa { id, .. } => {
                let cur = self.cur;
                self.write_variable(*id, cur, val);
            }
            LValue::Mem { addr, ast } => {
                let ty = scalar_ir_ty(ast).unwrap_or(IrTy::I64);
                self.emit(IrInst::Store {
                    ty,
                    addr: *addr,
                    val,
                });
            }
        }
    }

    // ---- coercion ----

    fn coerce(&mut self, val: Val, from: IrTy, to: IrTy) -> Val {
        if from == to {
            return val;
        }
        let dst = self.fresh_vreg();
        self.emit(IrInst::Cast {
            dst,
            to,
            from,
            src: val,
        });
        Val::Reg(dst)
    }

    /// Coerce to an AST target type, returning the resulting value (Bool normalises
    /// to 0/1).
    fn coerce_to_ast(&mut self, val: Val, from: IrTy, to: &Type) -> Result<Val, CodegenError> {
        Ok(self.coerce_to_ast_typed(val, from, to)?.0)
    }

    fn coerce_to_ast_typed(
        &mut self,
        val: Val,
        from: IrTy,
        to: &Type,
    ) -> Result<(Val, IrTy), CodegenError> {
        if matches!(to, Type::Bool) {
            let dst = self.fresh_vreg();
            let zero = if from.is_float() {
                Val::ImmF64(0)
            } else {
                Val::ImmInt(0)
            };
            self.emit(IrInst::Cmp {
                dst,
                op: CmpOp::Ne,
                ty: from,
                signed: false,
                lhs: val,
                rhs: zero,
            });
            return Ok((Val::Reg(dst), IrTy::U8));
        }
        let irty = scalar_ir_ty(to)
            .ok_or_else(|| CodegenError::new("coercion to a non-scalar type", None))?;
        Ok((self.coerce(val, from, irty), irty))
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

// ---- LValue accessors ----

fn lvalue_ast(lv: &LValue) -> &Type {
    match lv {
        LValue::Ssa { ast, .. } | LValue::Mem { ast, .. } => ast,
    }
}

fn lvalue_addr(lv: &LValue) -> Option<Val> {
    match lv {
        LValue::Mem { addr, .. } => Some(*addr),
        LValue::Ssa { .. } => None,
    }
}

// ---- pure helpers ----

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

fn ret_scalar(ret: IrRet) -> IrTy {
    match ret {
        IrRet::Scalar(t) => t,
        _ => IrTy::I64,
    }
}

fn deref_ty(ty: &Type) -> Option<&Type> {
    match ty {
        Type::Ptr(t) | Type::Array(t, _) => Some(t),
        _ => None,
    }
}

fn class_name(ty: &Type) -> Result<String, CodegenError> {
    match ty {
        Type::Named(n) => Ok(n.clone()),
        _ => Err(CodegenError::new("member access on a non-class", None)),
    }
}

fn is_ptr_like(e: &Expr) -> bool {
    matches!(e.ty(), Some(Type::Ptr(_)) | Some(Type::Array(..)))
}

fn is_f64(e: &Expr) -> bool {
    matches!(e.ty(), Some(Type::F64))
}

fn promoted(lhs: &Expr, rhs: &Expr) -> IrTy {
    if is_f64(lhs) || is_f64(rhs) {
        IrTy::F64
    } else {
        IrTy::I64
    }
}

fn type_signed(ty: &Type) -> bool {
    matches!(ty, Type::I8 | Type::I16 | Type::I32 | Type::I64)
}

fn expr_signed(e: &Expr) -> bool {
    e.ty().as_ref().is_none_or(type_signed)
}

fn signed_left(lhs: &Expr) -> bool {
    expr_signed(lhs)
}

fn signed_rel(lhs: &Expr, rhs: &Expr) -> bool {
    expr_signed(lhs) && expr_signed(rhs)
}

fn cmp_op(op: BinOp) -> Option<CmpOp> {
    Some(match op {
        BinOp::Eq => CmpOp::Eq,
        BinOp::Ne => CmpOp::Ne,
        BinOp::Lt => CmpOp::Lt,
        BinOp::Le => CmpOp::Le,
        BinOp::Gt => CmpOp::Gt,
        BinOp::Ge => CmpOp::Ge,
        _ => return None,
    })
}

fn arith_op(op: BinOp) -> Option<IrBinOp> {
    Some(match op {
        BinOp::Add => IrBinOp::Add,
        BinOp::Sub => IrBinOp::Sub,
        BinOp::Mul => IrBinOp::Mul,
        BinOp::Div => IrBinOp::Div,
        BinOp::Mod => IrBinOp::Mod,
        BinOp::BitAnd => IrBinOp::BitAnd,
        BinOp::BitOr => IrBinOp::BitOr,
        BinOp::BitXor => IrBinOp::BitXor,
        BinOp::Shl => IrBinOp::Shl,
        BinOp::Shr => IrBinOp::Shr,
        _ => return None,
    })
}

fn compound_binop(op: AssignOp) -> BinOp {
    match op {
        AssignOp::Assign => unreachable!("plain assign is not compound"),
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
    }
}

fn stmt_name(s: &StmtKind) -> &'static str {
    match s {
        StmtKind::Switch { .. } => "switch",
        StmtKind::Case { .. } => "case",
        StmtKind::Default => "default",
        StmtKind::SwitchStart => "start:",
        StmtKind::SwitchEnd => "end:",
        StmtKind::Try { .. } => "try",
        StmtKind::Throw(_) => "throw",
        StmtKind::Func(_) => "nested function",
        StmtKind::Class(_) => "class",
        _ => "statement",
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
