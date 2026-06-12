//! Unit tests for semantic analysis: whole-program checks via `check_program`
//! (unknown calls, undeclared variables, nominal aggregate compatibility) plus the
//! private type predicates (`is_integer`/`is_arithmetic`/`is_pointer`/`is_scalar`),
//! `decay`, `arith_result`, `is_anon_field`, and `array_dims_equal`. Programs are
//! built via `parser::parse` (which includes the implicit prelude).

use super::*;

/// Sema errors for `src`.
fn errors(src: &str) -> Vec<SemaError> {
    let prog = crate::parser::parse(src).expect("parse");
    check_program(&prog)
}

/// Assert `src` type-checks with no errors.
fn ok(src: &str) {
    let errs = errors(src);
    assert!(errs.is_empty(), "expected no sema errors, got: {errs:?}");
}

/// Assert `src` produces at least one sema error.
fn fails(src: &str) {
    assert!(!errors(src).is_empty(), "expected a sema error for: {src}");
}

#[test]
fn well_formed_program_has_no_errors() {
    ok("U0 Main() { I64 x = 5; x = x + 1; }");
}

#[test]
fn unknown_call_is_an_error() {
    // No implicit-extern fallback: an undeclared call cannot be resolved.
    fails("U0 Main() { NoSuchFunctionZq(); }");
}

#[test]
fn undeclared_variable_is_an_error() {
    fails("U0 Main() { I64 y = nonexistent_var_zq; }");
}

#[test]
fn nominal_aggregates_do_not_cross_assign() {
    // Two same-fielded but differently-named classes are distinct types.
    fails("class A { I64 x; } class B { I64 x; } U0 Main() { A a; B b; a = b; }");
    // The same named type assigns fine (by-value copy).
    ok("class A { I64 x; } U0 Main() { A a; A b; a = b; }");
}

// ---- type predicates & promotion (private helpers) ----

#[test]
fn integer_predicate() {
    for t in [
        Type::I8,
        Type::U8,
        Type::I16,
        Type::U16,
        Type::I32,
        Type::U32,
        Type::I64,
        Type::U64,
        Type::Bool,
    ] {
        assert!(is_integer(&t), "{t:?} should be integer");
    }
    assert!(!is_integer(&Type::F64));
    assert!(!is_integer(&Type::Ptr(Box::new(Type::I64))));
    assert!(!is_integer(&Type::U0));
}

#[test]
fn arithmetic_and_scalar_predicates() {
    assert!(is_arithmetic(&Type::F64));
    assert!(is_arithmetic(&Type::I32));
    assert!(!is_arithmetic(&Type::Ptr(Box::new(Type::U8))));

    assert!(is_scalar(&Type::Ptr(Box::new(Type::U8))));
    assert!(is_scalar(&Type::F64));
    assert!(!is_scalar(&Type::Named("Foo".into())));
}

#[test]
fn pointer_predicate_and_array_decay() {
    let arr = Type::Array(Box::new(Type::I32), None);
    assert!(is_pointer(&arr)); // arrays are pointer-like at use sites
    assert!(is_pointer(&Type::Ptr(Box::new(Type::I64))));
    assert!(!is_pointer(&Type::I64));
    // Array decays to a pointer to its element; non-arrays are unchanged.
    assert_eq!(decay(arr), Type::Ptr(Box::new(Type::I32)));
    assert_eq!(decay(Type::I64), Type::I64);
}

#[test]
fn arith_result_promotion() {
    // Float wins over integer.
    assert_eq!(arith_result(&Type::I64, &Type::F64), Type::F64);
    // A pointer operand makes the result that pointer.
    let p = Type::Ptr(Box::new(Type::U8));
    assert_eq!(arith_result(&p, &Type::I64), p);
    // Two integers ⇒ I64 (HolyC arithmetic is 64-bit).
    assert_eq!(arith_result(&Type::I32, &Type::U8), Type::I64);
}

#[test]
fn anon_field_detection() {
    assert!(is_anon_field("$anon0"));
    assert!(!is_anon_field("x"));
    assert!(!is_anon_field("anon"));
}

#[test]
fn array_dims_equal_folds_constants() {
    let dim = |v: i64| Some(Box::new(Expr::new(ExprKind::Int(v), Span::dummy())));
    assert!(array_dims_equal(&None, &None));
    assert!(array_dims_equal(&dim(4), &dim(4)));
    assert!(!array_dims_equal(&dim(4), &dim(5)));
    assert!(!array_dims_equal(&dim(4), &None));
}
