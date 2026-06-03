//! Tests for semantic analysis: name resolution, type checks, context rules.

use solomon::ast::{StmtKind, Type};
use solomon::parser::parse;
use solomon::sema::{SemaError, check_program};

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
    has(
        "U0 F() { Frobnicate(1); }",
        "call to undeclared function `Frobnicate`",
    );
}

#[test]
fn print_intrinsic_is_known() {
    // `Print` is a registered intrinsic (the interpreter implements it), so
    // calling it is not flagged even though there's no user definition.
    ok("U0 F() { Print(\"hello\"); }");
}

#[test]
fn stdlib_builtins_are_known() {
    // The remaining irreducible intrinsics are registered, so calls type-check and
    // yield their declared return types. (The reducible string/memory/ctype/PRNG
    // helpers moved to the `lib/*.hc` stdlib — they're ordinary library calls now,
    // covered by the `tests/stdlib.rs` conformance suite.)
    ok("U0 F() { F64 r = Sqrt(2.0); F64 fa = Fabs(-1.0); }");
    ok("U0 F() { \
          U8 *p = MAlloc(16); \
          F64 sq = Sqrt(2.0); F64 fa = Fabs(-1.0); \
          U8 *sp = StrPrint(p, \"%d\", 1); U8 *cp = CatPrint(p, \"%d\", 2); \
          U8 *mp = MStrPrint(\"%d\", 3); \
          I64 ns = UnixNS(); I64 mn = NanoNS(); Sleep(0); \
          Free(p); \
        }");
}

#[test]
fn stdlib_builtin_arity_is_checked() {
    has("U0 F() { Sqrt(1.0, 2.0); }", "got 2");
    has("U0 F() { MAlloc(); }", "expects 1 argument(s), got 0");
    has("U0 F() { Sqrt(); }", "expects 1 argument(s), got 0");
    has("U0 F() { StrPrint(0); }", "at least 2 argument(s), got 1");
    has("U0 F() { CatPrint(0); }", "at least 2 argument(s), got 1");
    has("U0 F() { MStrPrint(); }", "at least 1 argument(s), got 0");
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
    has(
        "class Derived : Missing { I64 x; }",
        "unknown base type `Missing`",
    );
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
    has("class P { I64 x; } U0 F() { P *p; p.x; }", "use `->`");
}

#[test]
fn member_access_on_call_result_is_ok() {
    // Reading a field off a struct-returning call is allowed.
    ok("class P{I64 x; I64 y;} P Mk(){ P p; return p; } U0 F() { I64 a = Mk().x; }");
}

#[test]
fn assigning_to_call_result_member_is_rejected() {
    // A call result is a temporary, so its member is not an lvalue.
    has(
        "class P{I64 x;} P Mk(){ P p; return p; } U0 F() { Mk().x = 1; }",
        "not an lvalue",
    );
}

#[test]
fn address_of_call_result_member_is_rejected() {
    has(
        "class P{I64 x;} P Mk(){ P p; return p; } U0 F() { I64 *q = &Mk().x; }",
        "non-lvalue",
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
    has("I64 One() { return 1; } U0 F() { One(1, 2); }", "got 2");
}

// ---- function pointers ----

#[test]
fn function_pointers_are_ok() {
    // declare, take a function's address, and call through it
    ok("I64 Add(I64 a, I64 b) { return a + b; } \
        U0 F() { I64 (*fp)(I64, I64) = &Add; fp(1, 2); }");
    // a callback parameter
    ok("I64 Add(I64 a, I64 b) { return a + b; } \
        I64 Apply(I64 (*op)(I64, I64), I64 x, I64 y) { return op(x, y); } \
        U0 F() { Apply(&Add, 1, 2); }");
    // reassignment between same-signature functions, and use in a condition
    ok("I64 A(I64 x) { return x; } I64 B(I64 x) { return -x; } \
        U0 F() { I64 (*fp)(I64) = &A; fp = &B; if (fp) fp(5); }");
}

#[test]
fn function_pointers_in_structs_and_arrays() {
    // A function-pointer struct field (vtable-style).
    ok("I64 Sq(I64 s) { return s * s; } \
        class Shape { I64 (*area)(I64); I64 size; } \
        U0 F() { Shape s = {&Sq, 5}; s.area(s.size); }");
    // An array of function pointers (a dispatch table), indexed and called.
    ok(
        "I64 Add(I64 a, I64 b) { return a + b; } I64 Mul(I64 a, I64 b) { return a * b; } \
        U0 F() { I64 (*ops[])(I64, I64) = {&Add, &Mul}; I64 i = 1; ops[i](2, 3); }",
    );
}

#[test]
fn calling_a_non_function_is_rejected() {
    has("U0 F() { I64 x; x(1); }", "called value is not a function");
}

// ---- typedef ----

#[test]
fn typedef_aliases_are_usable() {
    // a simple alias
    ok("typedef I64 MyInt; U0 F() { MyInt x = 1; x = x + 2; }");
    // a function-pointer alias, and a function that returns one
    ok("typedef I64 (*BinOp)(I64, I64); \
        I64 Add(I64 a, I64 b) { return a + b; } \
        BinOp Pick() { return &Add; } \
        U0 F() { BinOp op = Pick(); op(1, 2); }");
    // aliasing a class and an array, used as parameter/field types
    ok("class PS { I64 x; } typedef PS Pt; typedef I64 V3[3]; \
        I64 Sum(V3 v) { return v[0] + v[1] + v[2]; } \
        U0 F() { Pt p; p.x = 1; }");
}

// ---- embedded unions ----

#[test]
fn embedded_unions_type_check() {
    // an anonymous union promotes its members into the class
    ok("class R { I64 tag; union { I64 w; U8 b[8]; }; } \
        U0 F() { R r; r.tag = 1; r.w = 2; r.b[0] = 3; }");
    // named embedded unions: an inline member and a previously-defined one
    ok("union V { I64 i; } \
        class Box { union Bits { I64 w; } b; union V v; } \
        U0 F() { Box x; x.b.w = 1; x.v.i = 2; }");
}

#[test]
fn unknown_member_through_anonymous_union_is_an_error() {
    has(
        "class R { union { I64 w; }; } U0 F() { R r; r.nope = 1; }",
        "no field `nope`",
    );
}

#[test]
fn function_pointer_arity_is_checked() {
    has(
        "I64 Add(I64 a, I64 b) { return a + b; } \
         U0 F() { I64 (*fp)(I64, I64) = &Add; fp(1); }",
        "function pointer expects 2 argument(s), got 1",
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

#[test]
fn switch_start_end_sublabels_are_accepted() {
    ok("U0 F() { switch [1] { start: ; case 1: break; case 2: ; end: ; } }");
}

#[test]
fn start_end_outside_switch_are_rejected() {
    has("U0 F() { start: ; }", "`start` outside");
    has("U0 F() { end: ; }", "`end` outside");
}

#[test]
fn misplaced_switch_sublabels_are_rejected() {
    has(
        "U0 F() { switch (1) { case 1: ; start: ; } }",
        "`start:` must come before",
    );
    has(
        "U0 F() { switch (1) { end: ; case 1: ; } }",
        "`end:` must come after",
    );
    has(
        "U0 F() { switch (1) { start: ; start: ; case 1: ; } }",
        "duplicate `start:`",
    );
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

// ---- offset() ----

#[test]
fn offset_of_known_member_is_ok() {
    ok("class Pt{I64 x; I64 y;} class Box{Pt lo; Pt hi;} I64 g = offset(Box.hi.y);");
}

#[test]
fn offset_of_unknown_class() {
    has("I64 g = offset(Bogus.x);", "not a known class or union");
}

#[test]
fn offset_of_unknown_field() {
    has("class Pt{I64 x;} I64 g = offset(Pt.z);", "no field `z`");
}

#[test]
fn offset_cannot_descend_into_a_scalar_field() {
    has(
        "class Pt{I64 x;} I64 g = offset(Pt.x.y);",
        "is not a class, so `offset` cannot descend",
    );
}

// ---- aggregate initializers ----

#[test]
fn aggregate_initializers_are_ok() {
    ok("U0 F() { I64 a[] = {1, 2, 3}; I64 b[5] = {1, 2}; I64 m[2][2] = {{1,2},{3,4}}; }");
    ok("class Pt{I64 x; I64 y;} U0 F() { Pt p = {1, 2}; Pt ps[2] = {{1,2},{3,4}}; }");
}

#[test]
fn too_many_array_initializers() {
    has("U0 F() { I64 a[2] = {1, 2, 3}; }", "too many initializers");
}

#[test]
fn too_many_struct_initializers() {
    has(
        "class Pt{I64 x;} U0 F() { Pt p = {1, 2}; }",
        "too many initializers",
    );
}

#[test]
fn init_list_for_scalar_is_rejected() {
    has(
        "U0 F() { I64 x = {1, 2}; }",
        "can only initialize an array, class, or union",
    );
}

// ---- designated initializers ----

#[test]
fn designated_initializers_are_ok() {
    // Fields may appear out of order, be omitted, and nest.
    ok("class Pt{I64 x; I64 y;} U0 F() { Pt p = {.y = 2, .x = 1}; Pt q = {.x = 7}; }");
    ok("class Pt{I64 x; I64 y;} class Ln{Pt a; Pt b;} U0 F() { Ln l = {.b = {.x = 1, .y = 2}}; }");
    ok("class Pt{I64 x; I64 y;} Pt g = {.x = 1, .y = 2};");
}

#[test]
fn designated_init_unknown_field_is_rejected() {
    has(
        "class Pt{I64 x; I64 y;} U0 F() { Pt p = {.z = 1}; }",
        "has no field `z`",
    );
}

#[test]
fn designated_init_for_non_aggregate_is_rejected() {
    has(
        "U0 F() { I64 x = {.a = 1}; }",
        "can only initialize a class or union",
    );
    has(
        "U0 F() { I64 a[3] = {.a = 1}; }",
        "can only initialize a class or union",
    );
}

#[test]
fn designated_init_checks_field_type() {
    // The value must be assignable to the named field's type.
    has(
        "class Pt{I64 x; I64 y;} class Ln{Pt a;} U0 F() { Ln l = {.a = 5}; }",
        "cannot assign a scalar to class type `Pt`",
    );
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

#[test]
fn vararg_accessors_require_a_variadic_function() {
    has("U0 F() { VarArgI64(0); }", "only valid inside a variadic");
    ok("I64 F(...) { return VarArgCnt() + VarArgI64(0); }");
}
