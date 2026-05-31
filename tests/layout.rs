//! Tests for the type-layout pass: sizes, alignment, field offsets, padding,
//! inheritance, unions, and error cases (cyclic types, non-constant sizes).

use solomon::ast::Type;
use solomon::layout::{Layouts, compute};
use solomon::parser::parse;

/// Compute layouts for `src`, asserting there were no layout errors.
fn layouts(src: &str) -> Layouts {
    let program = parse(src).unwrap_or_else(|e| panic!("parse failed: {e}"));
    let (l, errs) = compute(&program);
    assert!(errs.is_empty(), "unexpected layout errors: {errs:?}");
    l
}

/// (offset, size) of a field.
fn field(l: &Layouts, class: &str, name: &str) -> (u64, u64) {
    let layout = l
        .get(class)
        .unwrap_or_else(|| panic!("no layout for `{class}`"));
    let f = layout
        .field_offset_size(name)
        .unwrap_or_else(|| panic!("no field `{name}` on `{class}`"));
    f
}

#[test]
fn simple_struct() {
    let l = layouts("class Point { I64 x; I64 y; }");
    let p = l.get("Point").unwrap();
    assert_eq!(p.size, 16);
    assert_eq!(p.align, 8);
    assert_eq!(l.offset_of("Point", "x"), Some(0));
    assert_eq!(l.offset_of("Point", "y"), Some(8));
}

#[test]
fn packed_bytes() {
    let l = layouts("class Pixel { U8 r; U8 g; U8 b; }");
    let p = l.get("Pixel").unwrap();
    assert_eq!(p.size, 3);
    assert_eq!(p.align, 1);
    assert_eq!(l.offset_of("Pixel", "r"), Some(0));
    assert_eq!(l.offset_of("Pixel", "g"), Some(1));
    assert_eq!(l.offset_of("Pixel", "b"), Some(2));
}

#[test]
fn padding_between_and_after_fields() {
    // U8 a; I64 b; U8 c;
    //   a @ 0, pad to 8, b @ 8, c @ 16, then round size up to align 8 => 24.
    let l = layouts("class Mixed { U8 a; I64 b; U8 c; }");
    let m = l.get("Mixed").unwrap();
    assert_eq!(l.offset_of("Mixed", "a"), Some(0));
    assert_eq!(l.offset_of("Mixed", "b"), Some(8));
    assert_eq!(l.offset_of("Mixed", "c"), Some(16));
    assert_eq!(m.size, 24);
    assert_eq!(m.align, 8);
}

#[test]
fn intermediate_alignment() {
    // U8 a; I32 b;  => a@0, pad to 4, b@4, size 8, align 4.
    let l = layouts("class S { U8 a; I32 b; }");
    let s = l.get("S").unwrap();
    assert_eq!(l.offset_of("S", "b"), Some(4));
    assert_eq!(s.size, 8);
    assert_eq!(s.align, 4);
}

#[test]
fn nested_struct_field() {
    let l = layouts(
        "class Inner { I32 x; }\n\
         class Outer { U8 tag; Inner inner; }",
    );
    // Inner: size 4, align 4. Outer: tag@0, pad to 4, inner@4, size 8.
    assert_eq!(l.get("Inner").unwrap().size, 4);
    assert_eq!(field(&l, "Outer", "inner"), (4, 4));
    assert_eq!(l.get("Outer").unwrap().size, 8);
}

#[test]
fn array_field_stride_and_size() {
    // I32 data[4] => 16 bytes at offset 0; U8 n @ 16; size rounds to 20 (align 4).
    let l = layouts("class Buf { I32 data[4]; U8 n; }");
    let b = l.get("Buf").unwrap();
    assert_eq!(field(&l, "Buf", "data"), (0, 16));
    assert_eq!(l.offset_of("Buf", "n"), Some(16));
    assert_eq!(b.size, 20);
    assert_eq!(b.align, 4);
}

#[test]
fn union_overlaps_fields() {
    let l = layouts("union Reg { I64 q; I32 d[2]; U8 b; }");
    let u = l.get("Reg").unwrap();
    assert!(u.is_union);
    assert_eq!(u.size, 8); // max(8, 8, 1)
    assert_eq!(u.align, 8);
    // All members start at 0.
    assert_eq!(l.offset_of("Reg", "q"), Some(0));
    assert_eq!(l.offset_of("Reg", "d"), Some(0));
    assert_eq!(l.offset_of("Reg", "b"), Some(0));
}

#[test]
fn inheritance_lays_base_first() {
    let l = layouts(
        "class Base { I64 a; }\n\
         class Derived : Base { I64 b; }",
    );
    let d = l.get("Derived").unwrap();
    assert_eq!(d.size, 16);
    assert_eq!(l.offset_of("Derived", "a"), Some(0)); // inherited
    assert_eq!(l.offset_of("Derived", "b"), Some(8));
}

#[test]
fn pointer_field_breaks_recursion() {
    // A self-referential type via a pointer is fine (the pointer is 8 bytes).
    let l = layouts("class Node { I64 v; Node *next; }");
    let n = l.get("Node").unwrap();
    assert_eq!(n.size, 16);
    assert_eq!(l.offset_of("Node", "next"), Some(8));
}

#[test]
fn size_and_align_of_scalar_types() {
    let l = Layouts::empty();
    assert_eq!(l.size_of(&Type::I64), 8);
    assert_eq!(l.size_of(&Type::U8), 1);
    assert_eq!(l.size_of(&Type::F64), 8);
    assert_eq!(l.size_of(&Type::Ptr(Box::new(Type::U8))), 8);
    assert_eq!(l.align_of(&Type::I32), 4);
}

// ---- error cases ----

#[test]
fn direct_cycle_is_an_error() {
    let program = parse("class A { A self; }").unwrap();
    let (_, errs) = compute(&program);
    assert!(
        errs.iter().any(|e| e.message.contains("infinite size")),
        "expected an infinite-size error, got: {errs:?}"
    );
}

#[test]
fn mutual_cycle_is_an_error() {
    let program = parse("class A { B b; } class B { A a; }").unwrap();
    let (_, errs) = compute(&program);
    assert!(
        errs.iter().any(|e| e.message.contains("infinite size")),
        "expected an infinite-size error, got: {errs:?}"
    );
}

#[test]
fn non_constant_field_array_size_is_an_error() {
    // `n` is not a constant, so the field array size can't be computed.
    let program = parse("class Bad { I64 a[n]; }").unwrap();
    let (_, errs) = compute(&program);
    assert!(
        errs.iter().any(|e| e.message.contains("constant")),
        "expected a constant-size error, got: {errs:?}"
    );
}

#[test]
fn constant_expression_field_size() {
    // A constant arithmetic expression is fine.
    let l = layouts("class Buf { U8 data[2 * 8]; }");
    assert_eq!(l.get("Buf").unwrap().size, 16);
}
