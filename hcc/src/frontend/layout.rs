//! Computes the in-memory size, alignment, and field offsets of every
//! `class`/`union`.
//!
//! `compute` is a standalone pass. Semantic analysis runs it and folds its
//! errors in; the backends consume its results. The interpreter uses it for
//! `sizeof`, and the AArch64 codegen backend uses offsets, sizes, and strides
//! for field access, array indexing, and pointer arithmetic.
//!
//! The layout model is natural alignment with padding, matching the x86-64 C
//! ABI:
//!   * Scalar alignments equal their sizes: `I8`=1, `I16`=2, `I32`=4, and
//!     `I64`/`U64`/`F64`/pointer=8.
//!   * Each field is placed at the next offset that is a multiple of its
//!     alignment, inserting padding as needed.
//!   * A class's alignment is the maximum of its fields' alignments. Its size is
//!     rounded up to that alignment, so arrays of it stay aligned.
//!   * A `union` places every field at offset 0. Its size is the largest field,
//!     rounded up to the maximum alignment.
//!   * A base class is laid out as a subobject at offset 0, before the derived
//!     fields.
//!
//! HolyC's exact rules are not authoritatively documented, so this follows the
//! standard x86-64 convention. The whole rule lives in [`align_of_scalar`] and
//! [`round_up`]. Switching to a packed layout (alignment 1) is a one-line change
//! if HolyC turns out to differ.

use std::collections::{HashMap, HashSet};

use crate::ast::{BinOp, ClassDef, Expr, ExprKind, Program, SizeofArg, StmtKind, Type, UnOp};
use crate::token::Pos;

/// An error discovered while computing layouts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutError {
    pub message: String,
    pub pos: Pos,
}

/// The layout of one field within an aggregate.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldLayout {
    pub name: String,
    pub ty: Type,
    pub offset: u64,
    pub size: u64,
}

/// The computed layout of a class or union.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub size: u64,
    pub align: u64,
    pub is_union: bool,
    /// Fields in offset order, including any inherited from base classes.
    pub fields: Vec<FieldLayout>,
}

impl Layout {
    fn field(&self, name: &str) -> Option<&FieldLayout> {
        self.fields.iter().find(|f| f.name == name)
    }

    /// The `(offset, size)` of a field by name.
    pub fn field_offset_size(&self, name: &str) -> Option<(u64, u64)> {
        self.field(name).map(|f| (f.offset, f.size))
    }
}

/// The layouts of all aggregate types in a program, plus size/alignment queries
/// for arbitrary types.
#[derive(Clone, Debug, Default)]
pub struct Layouts {
    classes: HashMap<String, Layout>,
}

impl Layouts {
    pub fn empty() -> Self {
        Layouts::default()
    }

    pub fn get(&self, name: &str) -> Option<&Layout> {
        self.classes.get(name)
    }

    /// The size in bytes of a type. Unknown class types report 0.
    pub fn size_of(&self, ty: &Type) -> u64 {
        match ty {
            Type::Named(n) => self.classes.get(n).map_or(0, |l| l.size),
            Type::Array(elem, Some(dim)) => {
                let count = const_eval_in(dim, Some(self))
                    .map(|v| v.max(0) as u64)
                    .unwrap_or(0);
                self.stride_of(elem) * count
            }
            Type::Array(_, None) => 0,
            other => scalar_size(other).unwrap_or(0),
        }
    }

    /// The alignment in bytes of a type.
    pub fn align_of(&self, ty: &Type) -> u64 {
        match ty {
            Type::Named(n) => self.classes.get(n).map_or(1, |l| l.align),
            Type::Array(elem, _) => self.align_of(elem),
            other => align_of_scalar(other),
        }
    }

    /// The per-element stride of `ty` as an array element: its size padded up to
    /// its alignment.
    pub fn stride_of(&self, ty: &Type) -> u64 {
        round_up(self.size_of(ty), self.align_of(ty))
    }

    /// The byte offset of `field` within `class`, if both exist.
    pub fn offset_of(&self, class: &str, field: &str) -> Option<u64> {
        self.classes
            .get(class)
            .and_then(|l| l.field(field))
            .map(|f| f.offset)
    }

    /// The byte offset of a possibly nested member `path` within `class`.
    ///
    /// For example, `["lo", "x"]` is the offset of `lo` plus the offset of `x`
    /// within `lo`'s type. Returns `None` if any class or field along the path is
    /// unknown, or if a non-final field is not itself a class or union.
    pub fn nested_offset_of(&self, class: &str, path: &[String]) -> Option<u64> {
        let mut current = class.to_string();
        let mut total = 0;
        for (i, field) in path.iter().enumerate() {
            let f = self.classes.get(&current)?.field(field)?;
            total += f.offset;
            if i + 1 < path.len() {
                match &f.ty {
                    Type::Named(n) => current = n.clone(),
                    _ => return None,
                }
            }
        }
        Some(total)
    }
}

/// Computes layouts for every class and union in `program`.
///
/// Returns the layouts plus any errors, such as cyclic by-value types or
/// non-constant field array sizes. The layout map is still populated on error,
/// with best-effort sizes, so callers can keep going.
pub fn compute(program: &Program) -> (Layouts, Vec<LayoutError>) {
    let mut defs: HashMap<String, (&ClassDef, Pos)> = HashMap::new();
    for item in &program.items {
        if let StmtKind::Class(c) = &item.kind {
            // On a duplicate definition, keep the first; sema already reports it.
            defs.entry(c.name.clone()).or_insert((c, item.span.pos));
        }
    }

    let mut cx = Computer {
        defs,
        out: Layouts::default(),
        visiting: HashSet::new(),
        errors: Vec::new(),
    };
    let names: Vec<String> = cx.defs.keys().cloned().collect();
    for name in names {
        cx.class_layout(&name);
    }
    (cx.out, cx.errors)
}

struct Computer<'p> {
    defs: HashMap<String, (&'p ClassDef, Pos)>,
    /// The layouts built so far. Held as a `Layouts` (not a bare map) so a `sizeof(class)`
    /// array dimension can be folded against it via [`const_eval_in`], once the referenced
    /// class has been forced (see `type_size`).
    out: Layouts,
    /// Classes currently being laid out, for cycle detection.
    visiting: HashSet<String>,
    errors: Vec<LayoutError>,
}

impl<'p> Computer<'p> {
    /// Lays out a class and memoises the result. Returns `(size, align)`.
    fn class_layout(&mut self, name: &str) -> (u64, u64) {
        if let Some(l) = self.out.get(name) {
            return (l.size, l.align);
        }
        let Some(&(def, def_pos)) = self.defs.get(name) else {
            // Unknown type. Sema reports this, so treat it as zero-size here.
            return (0, 1);
        };
        if !self.visiting.insert(name.to_string()) {
            self.errors.push(LayoutError {
                message: format!("type `{name}` has an infinite size (cycle through itself)"),
                pos: def_pos,
            });
            return (0, 1);
        }

        let mut fields = Vec::new();
        let mut offset = 0u64;
        let mut max_align = 1u64;

        // A base class is a subobject at offset 0; its fields are inherited.
        if let Some(base) = &def.base {
            let (bsize, balign) = self.class_layout(base);
            max_align = max_align.max(balign);
            if let Some(bl) = self.out.get(base) {
                fields.extend(bl.fields.iter().cloned());
            }
            offset = bsize;
        }

        for f in &def.fields {
            let a = self.type_align(&f.ty);
            let s = self.type_size(&f.ty, f.span.pos);
            max_align = max_align.max(a);
            let field_offset = if def.is_union { 0 } else { round_up(offset, a) };
            fields.push(FieldLayout {
                name: f.name.clone(),
                ty: f.ty.clone(),
                offset: field_offset,
                size: s,
            });
            // An anonymous embedded union promotes its members into this class at
            // the union's offset, so `obj.member` resolves to the correct slot.
            if crate::sema::is_anon_field(&f.name) {
                if let Type::Named(inner) = &f.ty {
                    self.class_layout(inner);
                    if let Some(inner_layout) = self.out.get(inner) {
                        let promoted: Vec<FieldLayout> = inner_layout
                            .fields
                            .iter()
                            .map(|mf| FieldLayout {
                                name: mf.name.clone(),
                                ty: mf.ty.clone(),
                                offset: field_offset + mf.offset,
                                size: mf.size,
                            })
                            .collect();
                        fields.extend(promoted);
                    }
                }
            }
            if def.is_union {
                offset = offset.max(s);
            } else {
                offset = field_offset + s;
            }
        }

        let size = round_up(offset, max_align);
        self.visiting.remove(name);
        self.out.classes.insert(
            name.to_string(),
            Layout {
                size,
                align: max_align,
                is_union: def.is_union,
                fields,
            },
        );
        (size, max_align)
    }

    fn type_align(&mut self, ty: &Type) -> u64 {
        match ty {
            Type::Named(n) => self.class_layout(n).1,
            Type::Array(elem, _) => self.type_align(elem),
            other => align_of_scalar(other),
        }
    }

    fn type_size(&mut self, ty: &Type, pos: Pos) -> u64 {
        match ty {
            Type::Named(n) => self.class_layout(n).0,
            Type::Array(elem, Some(dim)) => {
                let stride = round_up(self.type_size(elem, pos), self.type_align(elem));
                // A `sizeof(aggregate)` in the dimension (e.g. `U8 buf[sizeof(Other)]`) is a
                // compile-time constant, but folding it needs that aggregate's size, so force
                // each referenced class first; then fold `dim` against the layouts so far.
                let mut referenced = Vec::new();
                collect_sizeof_aggregates(dim, &mut referenced);
                for name in referenced {
                    self.class_layout(&name);
                }
                match const_eval_in(dim, Some(&self.out)) {
                    Ok(count) if count >= 0 => stride * count as u64,
                    Ok(_) => {
                        self.errors.push(LayoutError {
                            message: "array size cannot be negative".into(),
                            pos: dim.span.pos,
                        });
                        0
                    }
                    Err(e) => {
                        self.errors.push(e);
                        0
                    }
                }
            }
            Type::Array(_, None) => 0,
            other => scalar_size(other).unwrap_or(0),
        }
    }
}

/// Collect the base named aggregate of every `sizeof(non-scalar)` appearing in `e`, so the
/// layout pass can force those classes before folding `e` as an array dimension. Pointers
/// are skipped (a pointer's size never depends on its pointee's layout, so peeling through
/// one could wrongly force the enclosing class and trip cycle detection).
fn collect_sizeof_aggregates(e: &Expr, out: &mut Vec<String>) {
    match &e.kind {
        ExprKind::Sizeof(SizeofArg::Type(t)) => {
            if scalar_size(t).is_none() {
                if let Some(n) = base_named_type(t) {
                    out.push(n);
                }
            }
        }
        ExprKind::Unary { expr, .. } | ExprKind::Cast { expr, .. } => {
            collect_sizeof_aggregates(expr, out)
        }
        ExprKind::Binary { lhs, rhs, .. } => {
            collect_sizeof_aggregates(lhs, out);
            collect_sizeof_aggregates(rhs, out);
        }
        ExprKind::Ternary { cond, then, else_ } => {
            collect_sizeof_aggregates(cond, out);
            collect_sizeof_aggregates(then, out);
            collect_sizeof_aggregates(else_, out);
        }
        _ => {}
    }
}

/// The base named type of `t`, peeling only array wrappers (`Box[3]` → `Box`). A pointer
/// stops the peel: its size is fixed regardless of the pointee.
fn base_named_type(t: &Type) -> Option<String> {
    match t {
        Type::Named(n) => Some(n.clone()),
        Type::Array(inner, _) => base_named_type(inner),
        _ => None,
    }
}

// ---- scalar sizes & alignment (the layout rule lives here) ----

/// The size of a non-aggregate type, or `None` for class/array types.
pub(crate) fn scalar_size(ty: &Type) -> Option<u64> {
    Some(match ty {
        Type::U0 => 0,
        Type::I8 | Type::U8 | Type::Bool => 1,
        Type::I16 | Type::U16 => 2,
        Type::I32 | Type::U32 => 4,
        Type::I64 | Type::U64 | Type::F64 | Type::Ptr(_) | Type::FuncPtr { .. } => 8,
        Type::Named(_) | Type::Array(..) => return None,
        // Generics are monomorphized away before the layout pass runs.
        Type::Param(_) | Type::Generic(..) | Type::Tuple(_) => {
            unreachable!("unresolved generic type reached layout")
        }
    })
}

/// The alignment of a scalar type. Aggregates derive theirs from their fields.
fn align_of_scalar(ty: &Type) -> u64 {
    // Natural alignment: equal to the size, with a minimum of 1. Return `1` here
    // instead for a packed layout.
    scalar_size(ty).unwrap_or(1).max(1)
}

fn round_up(value: u64, align: u64) -> u64 {
    if align <= 1 {
        value
    } else {
        value.div_ceil(align) * align
    }
}

// ---- constant expression evaluation (for field array sizes) ----

/// Evaluates a compile-time constant integer expression.
///
/// Supports literals; arithmetic, bitwise, comparison, and logical operators;
/// integer casts; and `sizeof` of a scalar type. Anything else is rejected, such
/// as a variable or a call. (`mono` reuses this to evaluate `int`-parameter value
/// arguments, after it has substituted any value-param identifiers into `Int`
/// literals.) `sizeof` of a class/tuple/array needs the computed layout, so it is
/// only foldable through [`const_eval_in`]; this scalar-only form rejects it.
pub(crate) fn const_eval(e: &Expr) -> Result<i64, LayoutError> {
    const_eval_in(e, None)
}

/// Like [`const_eval`], but with optional layout context. With `layouts` supplied,
/// `sizeof(aggregate)` (a class, union, tuple, or array of them) folds to its computed
/// size — such a dimension is a genuine compile-time constant, just not a *scalar* one.
/// Without it (parse time, sema, mono value args) `sizeof` of a non-scalar still errors.
pub(crate) fn const_eval_in(e: &Expr, layouts: Option<&Layouts>) -> Result<i64, LayoutError> {
    let err = |msg: &str| LayoutError {
        message: msg.into(),
        pos: e.span.pos,
    };
    match &e.kind {
        ExprKind::Int(v) | ExprKind::Char(v) => Ok(*v),
        ExprKind::Unary { op, expr } => {
            let x = const_eval_in(expr, layouts)?;
            Ok(match op {
                UnOp::Neg => x.wrapping_neg(),
                UnOp::Pos => x,
                UnOp::Not => i64::from(x == 0),
                UnOp::BitNot => !x,
                _ => return Err(err("array size must be a constant integer expression")),
            })
        }
        ExprKind::Binary { op, lhs, rhs } => {
            let a = const_eval_in(lhs, layouts)?;
            let b = const_eval_in(rhs, layouts)?;
            use BinOp::*;
            Ok(match op {
                Add => a.wrapping_add(b),
                Sub => a.wrapping_sub(b),
                Mul => a.wrapping_mul(b),
                Div => {
                    if b == 0 {
                        return Err(err("division by zero in a constant expression"));
                    }
                    a.wrapping_div(b)
                }
                Mod => {
                    if b == 0 {
                        return Err(err("division by zero in a constant expression"));
                    }
                    a.wrapping_rem(b)
                }
                BitAnd => a & b,
                BitOr => a | b,
                BitXor => a ^ b,
                Shl => a.wrapping_shl(b as u32),
                Shr => a.wrapping_shr(b as u32),
                Eq => i64::from(a == b),
                Ne => i64::from(a != b),
                Lt => i64::from(a < b),
                Gt => i64::from(a > b),
                Le => i64::from(a <= b),
                Ge => i64::from(a >= b),
                And => i64::from(a != 0 && b != 0),
                Or => i64::from(a != 0 || b != 0),
            })
        }
        ExprKind::Ternary { cond, then, else_ } => {
            if const_eval_in(cond, layouts)? != 0 {
                const_eval_in(then, layouts)
            } else {
                const_eval_in(else_, layouts)
            }
        }
        // An integer cast is a no-op for constant folding.
        ExprKind::Cast { expr, .. } => const_eval_in(expr, layouts),
        // `sizeof(scalar)` (incl. pointers) always folds; `sizeof(aggregate)` folds only
        // with layout context (`sizeof(expr)` still needs type inference the layout pass
        // does not run, so it is rejected).
        ExprKind::Sizeof(SizeofArg::Type(t)) => {
            if let Some(s) = scalar_size(t) {
                Ok(s as i64)
            } else if let Some(l) = layouts {
                Ok(l.size_of(t) as i64)
            } else {
                Err(err("sizeof of a non-scalar type is not allowed here"))
            }
        }
        _ => Err(err("array size must be a constant integer expression")),
    }
}

#[cfg(test)]
#[path = "tests/layout.rs"]
mod tests;
