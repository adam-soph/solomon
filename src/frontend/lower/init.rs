//! Brace/designated initializer lowering for [`crate::lower::Lowerer`]: zero the aggregate, then
//! emit the leaf stores (`MemZero` + per-leaf `Store`, recursing into nested aggregates).

use crate::lower::*;

impl<'a> crate::lower::Lowerer<'a> {
    pub(super) fn init_memory(
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
    pub(super) fn lower_init_into(
        &mut self,
        addr: Val,
        ty: &Type,
        init: &Expr,
    ) -> Result<(), CodegenError> {
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
}
