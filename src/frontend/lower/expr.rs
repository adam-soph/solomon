//! Expression lowering for [`crate::lower::Lowerer`]: operators, calls, member/index access,
//! lvalue resolution, coercion, and condition-to-branch lowering.
use crate::lower::*;

impl<'a> crate::lower::Lowerer<'a> {
    pub(super) fn lower_cond(&mut self, e: &Expr) -> Result<Cond, CodegenError> {
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
    // ---- expressions ----

    pub(super) fn lower_expr(&mut self, e: &Expr) -> Result<(Val, IrTy), CodegenError> {
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
    /// args plus two hidden trailing args: `argc` (the variadic count) and `argv`
    /// (a pointer to a packed 8-byte-per-arg buffer).
    pub(super) fn lower_named_call(
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
            // The hidden `argc` (count) and `argv` (buffer) trailing arguments.
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
    pub(super) fn lower_aggregate_addr(&mut self, e: &Expr) -> Result<Val, CodegenError> {
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

    pub(super) fn coerce(&mut self, val: Val, from: IrTy, to: IrTy) -> Val {
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
    pub(super) fn coerce_to_ast(
        &mut self,
        val: Val,
        from: IrTy,
        to: &Type,
    ) -> Result<Val, CodegenError> {
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
}

// ---- helpers: operators, lvalues, type/representation mapping ----

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
