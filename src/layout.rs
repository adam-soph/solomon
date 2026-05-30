//! Type layout: the in-memory size, alignment, and field offsets of every
//! `class`/`union`.
//!
//! This is a standalone pass (`compute`) consumed by both semantic analysis
//! (which folds its errors in) and backends (the interpreter uses it for
//! `sizeof`; a future codegen backend would use offsets for field access and
//! pointer arithmetic).
//!
//! Layout model — **natural alignment with padding (the x86-64 C ABI)**:
//!   * scalar alignments equal their sizes (`I8`=1, `I16`=2, `I32`=4,
//!     `I64`/`U64`/`F64`/pointer=8),
//!   * each field is placed at the next offset that is a multiple of its
//!     alignment (inserting padding as needed),
//!   * a class's alignment is the maximum of its fields' alignments, and its
//!     size is rounded up to that alignment (trailing padding, so arrays stay
//!     aligned),
//!   * a `union` places every field at offset 0; its size is the largest field
//!     (rounded up to the max alignment),
//!   * a base class is laid out as a subobject at offset 0, before the derived
//!     fields.
//!
//! HolyC's exact rules are not authoritatively documented; this matches the
//! standard x86-64 convention. The whole rule lives in [`align_of_scalar`] /
//! [`round_up`], so switching to packed layout (alignment 1) is a one-line
//! change if HolyC turns out to differ.

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
                let count = const_eval(dim).map(|v| v.max(0) as u64).unwrap_or(0);
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

    /// The per-element stride of `ty` when used as an array element: its size
    /// padded up to its alignment.
    pub fn stride_of(&self, ty: &Type) -> u64 {
        round_up(self.size_of(ty), self.align_of(ty))
    }

    /// The byte offset of `field` within `class`, if both exist.
    pub fn offset_of(&self, class: &str, field: &str) -> Option<u64> {
        self.classes.get(class).and_then(|l| l.field(field)).map(|f| f.offset)
    }
}

/// Compute layouts for every class/union in `program`. Returns the layouts plus
/// any errors (cyclic by-value types, non-constant field array sizes). The
/// layout map is still populated on error (with best-effort sizes) so callers
/// can keep going.
pub fn compute(program: &Program) -> (Layouts, Vec<LayoutError>) {
    let mut defs: HashMap<String, (&ClassDef, Pos)> = HashMap::new();
    for item in &program.items {
        if let StmtKind::Class(c) = &item.kind {
            // On duplicate definitions (already a sema error) keep the first.
            defs.entry(c.name.clone()).or_insert((c, item.span.pos));
        }
    }

    let mut cx = Computer {
        defs,
        out: HashMap::new(),
        visiting: HashSet::new(),
        errors: Vec::new(),
    };
    let names: Vec<String> = cx.defs.keys().cloned().collect();
    for name in names {
        cx.class_layout(&name);
    }
    (Layouts { classes: cx.out }, cx.errors)
}

struct Computer<'p> {
    defs: HashMap<String, (&'p ClassDef, Pos)>,
    out: HashMap<String, Layout>,
    /// Classes currently being laid out, for cycle detection.
    visiting: HashSet<String>,
    errors: Vec<LayoutError>,
}

impl<'p> Computer<'p> {
    /// Lay out a class, memoising the result. Returns `(size, align)`.
    fn class_layout(&mut self, name: &str) -> (u64, u64) {
        if let Some(l) = self.out.get(name) {
            return (l.size, l.align);
        }
        let Some(&(def, def_pos)) = self.defs.get(name) else {
            // Unknown type — semantic analysis reports this; treat as zero-size.
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
            if def.is_union {
                offset = offset.max(s);
            } else {
                offset = field_offset + s;
            }
        }

        let size = round_up(offset, max_align);
        self.visiting.remove(name);
        self.out.insert(
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
                match const_eval(dim) {
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

// ---- scalar sizes & alignment (the layout rule lives here) ----

/// The size of a non-aggregate type, or `None` for class/array types.
fn scalar_size(ty: &Type) -> Option<u64> {
    Some(match ty {
        Type::U0 => 0,
        Type::I8 | Type::U8 | Type::Bool => 1,
        Type::I16 | Type::U16 => 2,
        Type::I32 | Type::U32 => 4,
        Type::I64 | Type::U64 | Type::F64 | Type::Ptr(_) => 8,
        Type::Named(_) | Type::Array(..) => return None,
    })
}

/// The alignment of a scalar type. (Aggregates derive theirs from their fields.)
fn align_of_scalar(ty: &Type) -> u64 {
    // Natural alignment: equal to the size, minimum 1. Switch this to `1` for a
    // packed layout.
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

/// Evaluate a compile-time constant integer expression. Supports literals,
/// arithmetic/bitwise/comparison/logical operators, integer casts, and
/// `sizeof` of a scalar type. Anything else (a variable, a call, `sizeof` of a
/// class) is rejected.
fn const_eval(e: &Expr) -> Result<i64, LayoutError> {
    let err = |msg: &str| LayoutError {
        message: msg.into(),
        pos: e.span.pos,
    };
    match &e.kind {
        ExprKind::Int(v) | ExprKind::Char(v) => Ok(*v),
        ExprKind::Unary { op, expr } => {
            let x = const_eval(expr)?;
            Ok(match op {
                UnOp::Neg => x.wrapping_neg(),
                UnOp::Pos => x,
                UnOp::Not => i64::from(x == 0),
                UnOp::BitNot => !x,
                _ => return Err(err("array size must be a constant integer expression")),
            })
        }
        ExprKind::Binary { op, lhs, rhs } => {
            let a = const_eval(lhs)?;
            let b = const_eval(rhs)?;
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
            if const_eval(cond)? != 0 {
                const_eval(then)
            } else {
                const_eval(else_)
            }
        }
        // An integer cast is a no-op for constant folding purposes.
        ExprKind::Cast { expr, .. } => const_eval(expr),
        // Only `sizeof(scalar-type)` is a constant we fold here. `sizeof(expr)`
        // needs type inference, which the layout pass doesn't run.
        ExprKind::Sizeof(SizeofArg::Type(t)) => scalar_size(t)
            .map(|s| s as i64)
            .ok_or_else(|| err("sizeof of a non-scalar type is not allowed here")),
        _ => Err(err("array size must be a constant integer expression")),
    }
}
