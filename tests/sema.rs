//! Tests for semantic analysis: name resolution, type checks, context rules.

use solomon::ast::{StmtKind, Type};
use solomon::parser::parse;
use solomon::sema::{check_program, SemaError};

/// Parse and analyze `src`, returning the semantic errors.
fn errs(src: &str) -> Vec<SemaError> {
    let program = parse(src).unwrap_or_else(|e| panic!("parse failed: {e}"));
    check_program(&program)
}

/// Assert `src` analyzes with no semantic errors.
fn ok(src: &str) {
    let e = errs(src);
    assert!(e.is_empty(), "expected no errors, got: {e:?}");
}

/// Assert some error message contains `needle`.
fn has(src: &str, needle: &str) {
    let e = errs(src);
    assert!(
        e.iter().any(|x| x.message.contains(needle)),
        "expected an error containing {needle:?}, got: {e:?}"
    );
}

// ---- things that should pass ----

#[test]
fn well_typed_program() {
    ok("I64 Fib(I64 n) { if (n < 2) return n; return Fib(n-1) + Fib(n-2); }");
}

#[test]
fn analysis_annotates_expression_types() {
    // After analysis the AST is typed: each expression carries its inferred
    // type, which backends (and sizeof) consume.
    let program = parse("F64 g = 1 + 2;").unwrap();
    assert!(check_program(&program).is_empty());
    let StmtKind::VarDecl { decls } = &program.items[0].kind else {
        panic!("expected a var decl");
    };
    let init = decls[0].init.as_ref().unwrap();
    // `1 + 2` is integer arithmetic => I64 (before assignment conversion).
    assert_eq!(init.ty(), Some(Type::I64));
}

#[test]
fn builtins_are_predefined() {
    ok("U0 F() { I64 *p = NULL; Bool b = TRUE; if (FALSE) return; }");
}

#[test]
fn nested_scopes_shadow_cleanly() {
    // A name declared in an inner block is gone once the block ends, so a fresh
    // declaration afterwards is fine.
    ok("U0 F() { { I64 a; } I64 a; }");
}

#[test]
fn default_and_vararg_arguments() {
    ok("\
        I64 G(I64 a, I64 b = 2) { return a + b; }\n\
        U0 P(U8 *fmt, ...);\n\
        U0 F() { G(1); G(1, 2); P(\"x\"); P(\"x\", 1, 2); }");
}

#[test]
fn member_access_through_value_and_pointer() {
    ok("\
        class Point { I64 x; I64 y; }\n\
        U0 F() { Point p; p.x = 1; Point *pp = &p; pp->y = 2; }");
}

#[test]
fn inherited_fields_are_visible() {
    ok("\
        class Base { I64 tag; }\n\
        class Derived : Base { I64 extra; }\n\
        U0 F() { Derived d; d.tag = 1; d.extra = 2; }");
}

#[test]
fn call_to_undeclared_function_is_an_error() {
    has("U0 F() { Frobnicate(1); }", "call to undeclared function `Frobnicate`");
}

#[test]
fn print_intrinsic_is_known() {
    // `Print` is a registered intrinsic (the interpreter implements it), so
    // calling it is not flagged even though there's no user definition.
    ok("U0 F() { Print(\"hello\"); }");
}

// ---- name resolution ----

#[test]
fn undeclared_variable_is_an_error() {
    has("U0 F() { y = 1; }", "undeclared identifier `y`");
}

#[test]
fn redeclaration_in_same_scope() {
    has("U0 F() { I64 a; I64 a; }", "redeclaration of `a`");
}

#[test]
fn unknown_type_in_field() {
    // An unknown type only reaches the analyzer where the parser accepts a bare
    // name as a type — i.e. a class field (in a statement it would already be a
    // parse error, since the parser can't know `Widget` is a type).
    has("class Box { Widget w; }", "unknown type `Widget`");
}

#[test]
fn unknown_base_class() {
    has("class Derived : Missing { I64 x; }", "unknown base type `Missing`");
}

#[test]
fn redefinition_of_function() {
    has(
        "I64 Add(I64 a) { return a; } I64 Add(I64 a) { return a; }",
        "redefinition of function `Add`",
    );
}

#[test]
fn redefinition_of_type() {
    has(
        "class P { I64 x; } class P { I64 y; }",
        "redefinition of type `P`",
    );
}

// ---- member access ----

#[test]
fn unknown_field() {
    has(
        "class P { I64 x; } U0 F() { P p; p.z = 1; }",
        "no field `z` on type `P`",
    );
}

#[test]
fn arrow_requires_pointer() {
    has(
        "class P { I64 x; } U0 F() { P p; p->x; }",
        "requires a pointer",
    );
}

#[test]
fn dot_on_pointer_is_an_error() {
    has(
        "class P { I64 x; } U0 F() { P *p; p.x; }",
        "use `->`",
    );
}

// ---- operator / type checks ----

#[test]
fn cannot_index_non_pointer() {
    has("U0 F() { I64 a; a[0]; }", "cannot index a non-pointer");
}

#[test]
fn cannot_deref_non_pointer() {
    has("U0 F() { I64 a; *a; }", "cannot dereference");
}

#[test]
fn address_of_non_lvalue() {
    has("U0 F() { &1; }", "address of a non-lvalue");
}

#[test]
fn assign_to_non_lvalue() {
    has("U0 F() { 1 = 2; }", "not an lvalue");
}

#[test]
fn bitwise_on_float_is_rejected() {
    has("U0 F() { F64 x = 1.0; x & 1; }", "integer operands");
}

#[test]
fn class_type_mismatch_in_assignment() {
    has(
        "class A { I64 x; } class B { I64 y; } U0 F() { A a; B b; a = b; }",
        "cannot assign",
    );
}

#[test]
fn non_scalar_condition() {
    has(
        "class C { I64 x; } U0 F() { C c; if (c) return; }",
        "condition must be a scalar",
    );
}

// ---- call arity ----

#[test]
fn too_few_arguments() {
    has(
        "I64 Add(I64 a, I64 b) { return a + b; } U0 F() { Add(1); }",
        "expects 2 argument(s), got 1",
    );
}

#[test]
fn too_many_arguments() {
    has(
        "I64 One() { return 1; } U0 F() { One(1, 2); }",
        "got 2",
    );
}

// ---- control-flow context ----

#[test]
fn break_outside_loop() {
    has("U0 F() { break; }", "`break` outside");
}

#[test]
fn continue_outside_loop() {
    has("U0 F() { continue; }", "`continue` outside");
}

#[test]
fn case_outside_switch() {
    has("U0 F() { case 1: ; }", "`case` outside");
}

#[test]
fn break_is_allowed_in_switch_and_loop() {
    ok("U0 F() { while (1) break; switch (1) { case 1: break; } }");
}

// ---- return checking ----

#[test]
fn value_returned_from_void() {
    has("U0 F() { return 1; }", "U0 (void) function");
}

#[test]
fn missing_return_value() {
    has("I64 F() { return; }", "missing return value");
}

// ---- goto / labels ----

#[test]
fn goto_undefined_label() {
    has("U0 F() { goto nowhere; }", "label `nowhere`");
}

#[test]
fn goto_into_a_sibling_block_is_out_of_scope() {
    // A label inside a block is not reachable by a goto outside that block.
    has(
        "U0 F() { { target: ; } goto target; }",
        "out-of-scope label `target`",
    );
}

#[test]
fn goto_to_a_label_in_an_enclosing_block_is_ok() {
    ok("U0 F() { done: { goto done; } }");
}

#[test]
fn goto_to_defined_label_is_ok() {
    ok("U0 F() { goto done; done: return; }");
}

// ---- positions ----

#[test]
fn errors_carry_positions() {
    // The undeclared use is on line 2.
    let src = "U0 F() {\n  zzz = 1;\n}";
    let e = errs(src);
    let undeclared = e
        .iter()
        .find(|x| x.message.contains("undeclared identifier `zzz`"))
        .expect("expected an undeclared-identifier error");
    assert_eq!(undeclared.pos.line, 2);
}
