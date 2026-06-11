//! Statement and control-flow lowering for [`crate::lower::Lowerer`]: blocks, if/while/for,
//! switch, try/throw, declarations, and the implicit-print statement forms.
use crate::lower::*;

impl<'a> crate::lower::Lowerer<'a> {
    // ---- statements ----

    pub(super) fn lower_stmt(&mut self, s: &Stmt) -> Result<(), CodegenError> {
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
        // allocation, which is not yet lowered — bail so the caller can fall back. A
        // `sizeof(aggregate)` dimension is constant (folds with the layout context), so it
        // is not a VLA; without `self.layouts` it would be misread as one.
        if let Type::Array(_, Some(dim)) = ty {
            if crate::layout::const_eval_in(dim, Some(self.layouts)).is_err() {
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
