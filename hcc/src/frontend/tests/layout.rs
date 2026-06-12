//! Unit tests for the layout pass: `repr(C)` natural alignment + padding, field
//! offsets, union overlap, base-class subobjects, nested member offsets, and the
//! private folding helpers (`round_up`, `align_of_scalar`, `scalar_size`,
//! `base_named_type`, `const_eval`). Programs are built via `parser::parse`, then
//! laid out with `compute`.

use super::*;
use crate::token::Span;

/// Parse `src` (with the implicit prelude) and return the computed layout of
/// `class`, asserting the layout pass reported no errors.
fn layout_of(src: &str, class: &str) -> Layout {
    let prog = crate::parser::parse(src).expect("parse");
    let (layouts, errs) = compute(&prog);
    assert!(errs.is_empty(), "layout errors: {errs:?}");
    layouts
        .get(class)
        .unwrap_or_else(|| panic!("no layout for `{class}`"))
        .clone()
}

/// Parse + lay out, returning the whole [`Layouts`] table.
fn layouts_of(src: &str) -> Layouts {
    let prog = crate::parser::parse(src).expect("parse");
    let (layouts, errs) = compute(&prog);
    assert!(errs.is_empty(), "layout errors: {errs:?}");
    layouts
}

#[test]
fn natural_alignment_inserts_padding() {
    // I8 at 0, then 7 bytes of padding, I64 at 8 ⇒ size 16, align 8.
    let l = layout_of("class P { I8 a; I64 b; }", "P");
    assert_eq!(l.align, 8);
    assert_eq!(l.size, 16);
    assert!(!l.is_union);
    assert_eq!(l.field_offset_size("a"), Some((0, 1)));
    assert_eq!(l.field_offset_size("b"), Some((8, 8)));
}

#[test]
fn fields_pack_in_declaration_order() {
    // I32@0, I16@4, I8@6 ⇒ end 7, rounded up to align 4 ⇒ size 8.
    let l = layout_of("class Q { I32 x; I16 y; I8 z; }", "Q");
    assert_eq!(l.align, 4);
    assert_eq!(l.size, 8);
    assert_eq!(l.field_offset_size("x"), Some((0, 4)));
    assert_eq!(l.field_offset_size("y"), Some((4, 2)));
    assert_eq!(l.field_offset_size("z"), Some((6, 1)));
}

#[test]
fn union_overlaps_all_fields_at_zero() {
    let l = layout_of("union U { I64 a; I8 b; I32 c; }", "U");
    assert!(l.is_union);
    assert_eq!(l.size, 8); // the largest field
    assert_eq!(l.align, 8);
    assert_eq!(l.field_offset_size("a"), Some((0, 8)));
    assert_eq!(l.field_offset_size("b"), Some((0, 1)));
    assert_eq!(l.field_offset_size("c"), Some((0, 4)));
}

#[test]
fn base_class_is_a_subobject_at_zero() {
    let ls = layouts_of("class Base { I64 a; } class Derived : Base { I64 b; }");
    assert_eq!(ls.offset_of("Derived", "a"), Some(0)); // inherited subobject
    assert_eq!(ls.offset_of("Derived", "b"), Some(8));
    assert_eq!(ls.get("Derived").unwrap().size, 16);
}

#[test]
fn nested_member_offsets() {
    let ls = layouts_of("class Inner { I32 a; I32 b; } class Outer { Inner lo; Inner hi; }");
    assert_eq!(ls.offset_of("Outer", "hi"), Some(8));
    assert_eq!(
        ls.nested_offset_of("Outer", &["hi".into(), "b".into()]),
        Some(12),
    );
    assert_eq!(ls.get("Outer").unwrap().size, 16); // two 8-byte Inners
}

#[test]
fn array_field_occupies_its_full_extent() {
    // I32 buf[4] ⇒ 16 bytes; class size rounds up to align 4.
    let l = layout_of("class A { I32 buf[4]; }", "A");
    assert_eq!(l.size, 16);
    assert_eq!(l.align, 4);
    assert_eq!(l.field_offset_size("buf"), Some((0, 16)));
}

#[test]
fn array_dimension_folds_constant_expression() {
    // Both `2 + 3` and `sizeof(I32)` fold to constants via const_eval.
    let l = layout_of("class A { I8 buf[2 + 3]; I8 raw[sizeof(I32)]; }", "A");
    assert_eq!(l.field_offset_size("buf"), Some((0, 5)));
    assert_eq!(l.field_offset_size("raw"), Some((5, 4)));
    assert_eq!(l.size, 9);
}

#[test]
fn cyclic_by_value_type_is_rejected() {
    // A class containing itself by value has no finite size.
    let prog = crate::parser::parse("class Loop { Loop inner; }").expect("parse");
    let (_layouts, errs) = compute(&prog);
    assert!(errs.iter().any(|e| e.message.contains("infinite size")));
}

// ---- the layout rule: free helpers ----

#[test]
fn round_up_rounds_to_alignment() {
    assert_eq!(round_up(0, 8), 0);
    assert_eq!(round_up(1, 8), 8);
    assert_eq!(round_up(8, 8), 8);
    assert_eq!(round_up(7, 4), 8);
    assert_eq!(round_up(5, 1), 5); // align <= 1 is a no-op
}

#[test]
fn scalar_sizes_match_the_abi() {
    assert_eq!(scalar_size(&Type::U0), Some(0));
    assert_eq!(scalar_size(&Type::I8), Some(1));
    assert_eq!(scalar_size(&Type::Bool), Some(1));
    assert_eq!(scalar_size(&Type::I16), Some(2));
    assert_eq!(scalar_size(&Type::I32), Some(4));
    assert_eq!(scalar_size(&Type::I64), Some(8));
    assert_eq!(scalar_size(&Type::F64), Some(8));
    assert_eq!(scalar_size(&Type::Ptr(Box::new(Type::U8))), Some(8));
    assert_eq!(scalar_size(&Type::Named("Foo".into())), None); // aggregate
}

#[test]
fn align_of_scalar_is_natural() {
    assert_eq!(align_of_scalar(&Type::I8), 1);
    assert_eq!(align_of_scalar(&Type::I16), 2);
    assert_eq!(align_of_scalar(&Type::I32), 4);
    assert_eq!(align_of_scalar(&Type::I64), 8);
    assert_eq!(align_of_scalar(&Type::F64), 8);
    assert_eq!(align_of_scalar(&Type::Named("Foo".into())), 1); // unknown ⇒ 1
}

#[test]
fn base_named_type_peels_arrays_not_pointers() {
    let arr = Type::Array(Box::new(Type::Named("Box".into())), None);
    assert_eq!(base_named_type(&arr), Some("Box".into()));
    let ptr = Type::Ptr(Box::new(Type::Named("Box".into())));
    assert_eq!(base_named_type(&ptr), None); // a pointer stops the peel
    assert_eq!(base_named_type(&Type::I32), None);
}

#[test]
fn const_eval_folds_integer_expressions() {
    let int = |v: i64| Expr::new(ExprKind::Int(v), Span::dummy());
    let bin = |op, l, r| {
        Expr::new(
            ExprKind::Binary {
                op,
                lhs: Box::new(l),
                rhs: Box::new(r),
            },
            Span::dummy(),
        )
    };
    // 2 + 3 * 4 = 14 (precedence is already baked into the tree shape).
    assert_eq!(
        const_eval(&bin(BinOp::Add, int(2), bin(BinOp::Mul, int(3), int(4)))),
        Ok(14),
    );
    // sizeof of a scalar folds without layout context.
    assert_eq!(
        const_eval(&Expr::new(
            ExprKind::Sizeof(SizeofArg::Type(Type::I32)),
            Span::dummy(),
        )),
        Ok(4),
    );
    // Division by zero is an error, not a panic.
    assert!(const_eval(&bin(BinOp::Div, int(1), int(0))).is_err());
    // A bare identifier is not a compile-time constant.
    assert!(const_eval(&Expr::new(ExprKind::Ident("x".into()), Span::dummy())).is_err());
}
