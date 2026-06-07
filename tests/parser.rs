//! Unit tests for the parser, asserting concrete AST shapes.
//!
//! AST equality ignores spans (see `ast.rs`), so expected trees are built with
//! [`Span::dummy`] via the `e`/`b` helpers below.

use solomon::ast::*;
use solomon::lexer::Lexer;
use solomon::parser::{Parser, parse};
use solomon::token::Span;

/// Parse a program, unwrapping the result. Drops the implicit `builtin.hc` prelude
/// items so structural assertions see only the user source plus the items mono
/// generated for it (the latter carry the `GENERATED_FILE` sentinel `Span::file`).
fn prog(src: &str) -> Program {
    let mut p = parse(src).unwrap_or_else(|e| panic!("parse failed: {e}"));
    p.items
        .retain(|s| s.span.file == 0 || s.span.file == solomon::token::GENERATED_FILE);
    p
}

/// Parse `src` (must be exactly one statement) and return that statement's kind.
fn stmt(src: &str) -> StmtKind {
    let mut p = prog(src);
    assert_eq!(p.items.len(), 1, "expected exactly one statement");
    p.items.pop().unwrap().kind
}

/// Parse `src` as a single expression statement and return the expression kind.
fn expr(src: &str) -> ExprKind {
    match stmt(&format!("{src};")) {
        StmtKind::Expr(e) => e.kind,
        other => panic!("expected expression statement, got {other:?}"),
    }
}

/// Wrap an `ExprKind` in an `Expr` with a placeholder span.
fn e(kind: ExprKind) -> Expr {
    Expr::new(kind, Span::dummy())
}

/// Boxed `e`.
fn b(kind: ExprKind) -> Box<Expr> {
    Box::new(e(kind))
}

fn bin(op: BinOp, lhs: ExprKind, rhs: ExprKind) -> ExprKind {
    ExprKind::Binary {
        op,
        lhs: b(lhs),
        rhs: b(rhs),
    }
}

fn ident(name: &str) -> ExprKind {
    ExprKind::Ident(name.into())
}

#[test]
fn parser_takes_a_lexer() {
    // The public entry point takes a lexer. Tokens are pulled on demand rather
    // than collected up front.
    let lexer = Lexer::new("I64 x = 1;");
    let mut parser = Parser::new(lexer);
    let program = parser.parse_program().unwrap();
    assert_eq!(program.items.len(), 1);
}

#[test]
fn nodes_carry_spans() {
    // `  42 ;` — the expression statement should span the `42`, not the spaces.
    let s = &prog("  42 ;").items[0];
    let StmtKind::Expr(inner) = &s.kind else {
        panic!("expected expr stmt");
    };
    let src = "  42 ;";
    assert_eq!(&src[inner.span.start..inner.span.end], "42");
    assert_eq!(inner.span.pos.line, 1);
    assert_eq!(inner.span.pos.col, 3);
}

#[test]
fn precedence_multiplication_binds_tighter_than_addition() {
    // 1 + 2 * 3  ==  1 + (2 * 3)
    assert_eq!(
        expr("1 + 2 * 3"),
        bin(
            BinOp::Add,
            ExprKind::Int(1),
            bin(BinOp::Mul, ExprKind::Int(2), ExprKind::Int(3)),
        )
    );
}

#[test]
fn left_associativity_of_subtraction() {
    // 10 - 3 - 2  ==  (10 - 3) - 2
    assert_eq!(
        expr("10 - 3 - 2"),
        bin(
            BinOp::Sub,
            bin(BinOp::Sub, ExprKind::Int(10), ExprKind::Int(3)),
            ExprKind::Int(2),
        )
    );
}

#[test]
fn comparison_and_logical_precedence() {
    // a < b && c == d  ==  (a < b) && (c == d)
    assert_eq!(
        expr("a < b && c == d"),
        bin(
            BinOp::And,
            bin(BinOp::Lt, ident("a"), ident("b")),
            bin(BinOp::Eq, ident("c"), ident("d")),
        )
    );
}

#[test]
fn chained_comparison_desugars_to_conjunction() {
    // HolyC `a < b < c` parses as `(a < b) && (b < c)`. The interior operand `b`
    // is duplicated into both comparisons.
    assert_eq!(
        expr("a < b < c"),
        bin(
            BinOp::And,
            bin(BinOp::Lt, ident("a"), ident("b")),
            bin(BinOp::Lt, ident("b"), ident("c")),
        )
    );
    // A longer chain folds left: `a < b <= c > d` -> ((a<b && b<=c) && c>d).
    assert_eq!(
        expr("a < b <= c > d"),
        bin(
            BinOp::And,
            bin(
                BinOp::And,
                bin(BinOp::Lt, ident("a"), ident("b")),
                bin(BinOp::Le, ident("b"), ident("c")),
            ),
            bin(BinOp::Gt, ident("c"), ident("d")),
        )
    );
    // Parentheses suppress chaining: `(a < b) < c` is a plain comparison.
    assert_eq!(
        expr("(a < b) < c"),
        bin(
            BinOp::Lt,
            bin(BinOp::Lt, ident("a"), ident("b")),
            ident("c")
        ),
    );
    // Equality does not chain: `a == b == c` keeps C's `(a == b) == c`.
    assert_eq!(
        expr("a == b == c"),
        bin(
            BinOp::Eq,
            bin(BinOp::Eq, ident("a"), ident("b")),
            ident("c")
        ),
    );
}

#[test]
fn assignment_is_right_associative() {
    // a = b = 1  ==  a = (b = 1)
    assert_eq!(
        expr("a = b = 1"),
        ExprKind::Assign {
            op: AssignOp::Assign,
            target: b(ident("a")),
            value: b(ExprKind::Assign {
                op: AssignOp::Assign,
                target: b(ident("b")),
                value: b(ExprKind::Int(1)),
            }),
        }
    );
}

#[test]
fn unary_and_postfix() {
    // -a++  parses as  -(a++): postfix binds tighter than prefix negate.
    assert_eq!(
        expr("-a++"),
        ExprKind::Unary {
            op: UnOp::Neg,
            expr: b(ExprKind::Postfix {
                op: PostOp::Inc,
                expr: b(ident("a")),
            }),
        }
    );
}

#[test]
fn member_index_and_call_chain() {
    // obj.items[i]->run(1)
    let ExprKind::Call { callee, args } = expr("obj.items[i]->run(1)") else {
        panic!("expected a call");
    };
    assert_eq!(args, vec![e(ExprKind::Int(1))]);
    let ExprKind::Member { arrow, field, .. } = callee.kind else {
        panic!("expected ->run member");
    };
    assert!(arrow);
    assert_eq!(field, "run");
}

#[test]
fn ternary_expression() {
    assert_eq!(
        expr("x ? 1 : 2"),
        ExprKind::Ternary {
            cond: b(ident("x")),
            then: b(ExprKind::Int(1)),
            else_: b(ExprKind::Int(2)),
        }
    );
}

#[test]
fn top_level_print_is_a_comma_expression() {
    // HolyC's implicit print: a format string followed by args.
    match stmt(r#""x=%d\n", x;"#) {
        StmtKind::Expr(Expr {
            kind: ExprKind::Comma(items),
            ..
        }) => {
            assert_eq!(items.len(), 2);
            assert_eq!(items[0].kind, ExprKind::Str("x=%d\n".into()));
            assert_eq!(items[1].kind, ident("x"));
        }
        other => panic!("expected comma expression, got {other:?}"),
    }
}

#[test]
fn variable_declaration_list_with_pointers_and_init() {
    // I64 *p, q = 5;  =>  two declarators sharing base I64, only the first a ptr.
    match stmt("I64 *p, q = 5;") {
        StmtKind::VarDecl { decls } => {
            assert_eq!(decls.len(), 2);
            assert_eq!(decls[0].name, "p");
            assert_eq!(decls[0].ty, Type::Ptr(Box::new(Type::I64)));
            assert_eq!(decls[0].init, None);
            assert_eq!(decls[1].name, "q");
            assert_eq!(decls[1].ty, Type::I64);
            assert_eq!(decls[1].init, Some(e(ExprKind::Int(5))));
        }
        other => panic!("expected var decl, got {other:?}"),
    }
}

#[test]
fn array_declaration() {
    match stmt("I32 grid[3][4];") {
        StmtKind::VarDecl { decls } => {
            // Outermost dimension is 3, inner is 4.
            let ty = &decls[0].ty;
            let Type::Array(inner, Some(dim0)) = ty else {
                panic!("expected outer array");
            };
            assert_eq!(dim0.kind, ExprKind::Int(3));
            let Type::Array(base, Some(dim1)) = inner.as_ref() else {
                panic!("expected inner array");
            };
            assert_eq!(dim1.kind, ExprKind::Int(4));
            assert_eq!(**base, Type::I32);
        }
        other => panic!("expected var decl, got {other:?}"),
    }
}

#[test]
fn function_definition_with_params_and_varargs() {
    let StmtKind::Func(f) = stmt("U0 Log(U8 *fmt, I64 level = 0, ...) { return; }") else {
        panic!("expected function");
    };
    assert_eq!(f.name, "Log");
    assert_eq!(f.ret, Type::U0);
    assert!(f.varargs);
    assert_eq!(f.params.len(), 2);
    assert_eq!(f.params[0].name.as_deref(), Some("fmt"));
    assert_eq!(f.params[0].ty, Type::Ptr(Box::new(Type::U8)));
    assert_eq!(f.params[1].name.as_deref(), Some("level"));
    assert_eq!(f.params[1].default, Some(e(ExprKind::Int(0))));
    assert!(f.body.is_some());
}

#[test]
fn function_prototype_has_no_body() {
    let StmtKind::Func(f) = stmt("I64 Add(I64 a, I64 b);") else {
        panic!("expected function");
    };
    assert!(f.body.is_none());
    assert_eq!(f.params.len(), 2);
}

#[test]
fn class_definition_registers_a_usable_type() {
    let p = prog(
        "class Vec { F64 x; F64 y; };\n\
         Vec MakeVec() { Vec v; return v; }",
    );
    // First item: the class with two fields.
    let StmtKind::Class(c) = &p.items[0].kind else {
        panic!("expected class");
    };
    assert_eq!(c.name, "Vec");
    assert!(!c.is_union);
    assert_eq!(c.fields.len(), 2);
    assert_eq!(c.fields[0].name, "x");
    assert_eq!(c.fields[0].ty, Type::F64);

    // Second item: a function whose return type and local both use `Vec`,
    // proving the type name was registered during class parsing.
    let StmtKind::Func(f) = &p.items[1].kind else {
        panic!("expected function");
    };
    assert_eq!(f.ret, Type::Named("Vec".into()));
}

#[test]
fn self_referential_class() {
    let StmtKind::Class(c) = stmt("class Node { I64 v; Node *next; };") else {
        panic!("expected class");
    };
    assert_eq!(c.fields[1].name, "next");
    assert_eq!(
        c.fields[1].ty,
        Type::Ptr(Box::new(Type::Named("Node".into())))
    );
}

#[test]
fn anonymous_class_in_declaration_synthesizes_a_type() {
    // `class { … } v;` declares `v` with a synthetic, structurally-named class type,
    // emitted as a top-level definition before the declaration.
    let p = prog("class { I64 x; I64 y; } v;");
    let StmtKind::Class(c) = &p.items[0].kind else {
        panic!("expected a synthetic class, got {:?}", p.items[0].kind);
    };
    assert!(c.name.starts_with("$Cls"), "name was {}", c.name);
    assert!(!c.is_union);
    assert_eq!(c.fields.len(), 2);
    let StmtKind::VarDecl { decls } = &p.items[1].kind else {
        panic!("expected a var decl");
    };
    assert_eq!(decls[0].ty, Type::Named(c.name.clone()));
}

#[test]
fn anonymous_union_uses_a_distinct_prefix() {
    let p = prog("union { I64 i; F64 f; } v;");
    let StmtKind::Class(c) = &p.items[0].kind else {
        panic!("expected a synthetic union");
    };
    assert!(c.is_union);
    assert!(c.name.starts_with("$ClsU"), "name was {}", c.name);
}

#[test]
fn identical_anonymous_classes_share_one_definition() {
    // The same field signature in two places dedups to a single synthetic type.
    let p = prog("class { I64 x; } a; class { I64 x; } b;");
    let n = p
        .items
        .iter()
        .filter(|s| matches!(&s.kind, StmtKind::Class(_)))
        .count();
    assert_eq!(
        n, 1,
        "identical anonymous classes should share one definition"
    );
}

#[test]
fn anonymous_classes_differing_in_array_dim_are_distinct() {
    // The mangled name folds in the array dimension, so `[4]` and `[8]` don't collide.
    let p = prog("class { I64 a[4]; } x; class { I64 a[8]; } y;");
    let n = p
        .items
        .iter()
        .filter(|s| matches!(&s.kind, StmtKind::Class(_)))
        .count();
    assert_eq!(n, 2, "different array dimensions must name different types");
}

#[test]
fn typedef_of_anonymous_class_is_usable() {
    // `typedef class { … } Name;` makes `Name` a usable alias of the anonymous type.
    let p = prog("typedef class { I64 x; I64 y; } P; P MakeP() { P p; return p; }");
    let cls = p
        .items
        .iter()
        .find_map(|s| match &s.kind {
            StmtKind::Class(c) => Some(c),
            _ => None,
        })
        .expect("expected a synthetic class");
    let f = p
        .items
        .iter()
        .find_map(|s| match &s.kind {
            StmtKind::Func(f) => Some(f),
            _ => None,
        })
        .expect("expected a function");
    assert_eq!(f.ret, Type::Named(cls.name.clone()));
}

#[test]
fn anonymous_aggregate_in_template_referencing_param_errors() {
    let err =
        parse("class Box<type T> { class { T v; } inner; } U0 M() { Box<I64> b; }").unwrap_err();
    assert!(
        err.message
            .contains("anonymous class/union types are not supported inside a generic template"),
        "got: {}",
        err.message
    );
}

#[test]
fn if_else_chain() {
    let StmtKind::If { cond, then, else_ } = stmt("if (x > 0) return 1; else return 0;") else {
        panic!("expected if");
    };
    assert_eq!(cond.kind, bin(BinOp::Gt, ident("x"), ExprKind::Int(0)));
    assert_eq!(then.kind, StmtKind::Return(Some(e(ExprKind::Int(1)))));
    assert_eq!(
        else_.map(|s| s.kind),
        Some(StmtKind::Return(Some(e(ExprKind::Int(0)))))
    );
}

#[test]
fn for_loop_parts() {
    let StmtKind::For {
        init,
        cond,
        step,
        body,
    } = stmt("for (I64 i = 0; i < n; i++) sum += i;")
    else {
        panic!("expected for");
    };
    // init is a declaration.
    assert!(matches!(
        init.map(|s| s.kind),
        Some(StmtKind::VarDecl { .. })
    ));
    assert!(cond.is_some());
    assert!(step.is_some());
    assert!(matches!(body.kind, StmtKind::Expr(_)));
}

#[test]
fn switch_with_range_case() {
    let StmtKind::Switch { body, .. } = stmt("switch (n) { case 1 ... 3: break; default: break; }")
    else {
        panic!("expected switch");
    };
    let StmtKind::Block(stmts) = body.kind else {
        panic!("expected block body");
    };
    // First label is a range case 1..=3.
    let StmtKind::Case { lo, hi } = &stmts[0].kind else {
        panic!("expected case label");
    };
    assert_eq!(lo.kind, ExprKind::Int(1));
    assert_eq!(hi.as_ref().map(|h| &h.kind), Some(&ExprKind::Int(3)));
    assert!(stmts.iter().any(|s| matches!(s.kind, StmtKind::Default)));
}

#[test]
fn cast_versus_grouping() {
    // (I64)x is a cast; (x) is just grouping.
    assert_eq!(
        expr("(I64)x"),
        ExprKind::Cast {
            ty: Type::I64,
            expr: b(ident("x")),
        }
    );
    assert_eq!(expr("(x)"), ident("x"));
}

#[test]
fn sizeof_of_a_type() {
    assert_eq!(
        expr("sizeof(I64)"),
        ExprKind::Sizeof(SizeofArg::Type(Type::I64))
    );
    assert_eq!(
        expr("sizeof(U8 *)"),
        ExprKind::Sizeof(SizeofArg::Type(Type::Ptr(Box::new(Type::U8))))
    );
}

#[test]
fn sizeof_of_an_expression() {
    // A non-type operand parses as `sizeof(expr)`.
    assert_eq!(
        expr("sizeof(x)"),
        ExprKind::Sizeof(SizeofArg::Expr(b(ident("x"))))
    );
}

#[test]
fn labels_and_goto() {
    let p = prog("U0 F() { goto done; done: return; }");
    let StmtKind::Func(f) = &p.items[0].kind else {
        panic!("expected function");
    };
    let body = f.body.as_ref().unwrap();
    assert_eq!(body[0].kind, StmtKind::Goto("done".into()));
    assert_eq!(body[1].kind, StmtKind::Label("done".into()));
}

#[test]
fn errors_carry_position() {
    // Missing semicolon.
    let e = parse("I64 x = 1").unwrap_err();
    assert!(e.message.contains("expected"), "msg was: {}", e.message);

    // Unclosed brace.
    assert!(parse("U0 F() {").is_err());

    // Garbage expression.
    assert!(parse("1 + ;").is_err());
}

#[test]
fn deeply_nested_input_is_an_error_not_a_stack_overflow() {
    // Pathologically deep nesting must surface as a recoverable parse error, not
    // overflow the native stack and abort the process. This runs on an 8 MiB
    // stack to match the real main-thread entry point (the `hcc`/`hci` CLIs).
    // The Rust test harness's smaller per-test thread would itself blow the
    // stack while reaching the depth cap, which is exactly the case the guard
    // converts into a clean error on the production path.
    let handle = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let src = format!("I64 x = {}1{};", "(".repeat(5000), ")".repeat(5000));
            let e = parse(&src).unwrap_err();
            assert!(
                e.message.contains("nested too deeply"),
                "msg was: {}",
                e.message
            );
        })
        .unwrap();
    handle.join().unwrap();
}

// ---- generic classes (monomorphization) ----

#[test]
fn generic_class_monomorphizes() {
    // `class Vec<type T>` plus a use `Vec<I64>` stamps out a concrete class with `T`
    // substituted. Its name is `3VecI64`, the injective mangling of `Vec<I64>`.
    // The template itself emits no class.
    let p = prog("class Vec<type T> { T *data; I64 len; } Vec<I64> x;");
    let classes: Vec<&ClassDef> = p
        .items
        .iter()
        .filter_map(|s| match &s.kind {
            StmtKind::Class(c) => Some(c),
            _ => None,
        })
        .collect();
    let vi = classes
        .iter()
        .find(|c| c.name == "3VecI64")
        .expect("Vec<I64> instance should be generated");
    assert_eq!(vi.fields[0].name, "data");
    assert_eq!(vi.fields[0].ty, Type::Ptr(Box::new(Type::I64)));
    assert_eq!(vi.fields[1].ty, Type::I64);
    assert!(!classes.iter().any(|c| c.name == "Vec")); // template emits nothing
}

#[test]
fn generic_dedups_repeated_instantiations() {
    let p = prog("class Vec<type T> { T *d; } Vec<I64> a; Vec<I64> b;");
    let n = p
        .items
        .iter()
        .filter(|s| matches!(&s.kind, StmtKind::Class(c) if c.name == "3VecI64"))
        .count();
    assert_eq!(n, 1, "Vec<I64> used twice should mint one class");
}

// ---- generic parameter kinds: `type` / `comparable` / `int` value params ----

/// All generated/concrete class names in a parsed program.
fn class_names(p: &Program) -> Vec<&str> {
    p.items
        .iter()
        .filter_map(|s| match &s.kind {
            StmtKind::Class(c) => Some(c.name.as_str()),
            _ => None,
        })
        .collect()
}

fn find_class<'a>(p: &'a Program, name: &str) -> Option<&'a ClassDef> {
    p.items.iter().find_map(|s| match &s.kind {
        StmtKind::Class(c) if c.name == name => Some(c),
        _ => None,
    })
}

#[test]
fn value_param_class_monomorphizes() {
    // `FixedArr<type T, int N>` with `FixedArr<I64, 8>` mints `8FixedArrI64C8E`, whose
    // `data` field is `I64[8]` — the value param `N` folded into the array dimension.
    let p = prog("class FixedArr<type T, int N> { T data[N]; I64 len; } FixedArr<I64, 8> x;");
    let c = find_class(&p, "8FixedArrI64C8E").expect("FixedArr<I64,8> instance");
    assert_eq!(c.fields[0].name, "data");
    assert_eq!(
        c.fields[0].ty,
        Type::Array(Box::new(Type::I64), Some(Box::new(e(ExprKind::Int(8)))))
    );
    assert_eq!(c.fields[1].ty, Type::I64);
}

#[test]
fn value_param_distinct_instances_and_dedup() {
    let p = prog(
        "class FixedArr<type T, int N> { T data[N]; } \
         FixedArr<I64, 8> a; FixedArr<I64, 9> b; FixedArr<I64, 8> c;",
    );
    let names = class_names(&p);
    assert!(names.contains(&"8FixedArrI64C8E"));
    assert!(names.contains(&"8FixedArrI64C9E")); // a different N is a different type
    assert_eq!(
        names.iter().filter(|n| **n == "8FixedArrI64C8E").count(),
        1,
        "`<I64,8>` used twice should mint one class"
    );
}

#[test]
fn type_keyword_is_synonym_for_bare_param() {
    // `class Box<type T>` behaves exactly like `class Box<type T>`.
    let p = prog("class Box<type T> { T v; } Box<I64> x;");
    let c = find_class(&p, "3BoxI64").expect("Box<I64> instance");
    assert_eq!(c.fields[0].ty, Type::I64);
}

#[test]
fn comparable_constraint_rejects_a_class() {
    let err = parse(
        "class Pt { I64 x; } T Max<comparable T>(T a, T b) { return a; } \
         U0 M() { Pt p; Pt q; Max(p, q); }",
    )
    .unwrap_err();
    assert!(err.to_string().contains("not comparable"), "got: {err}");
}

#[test]
fn comparable_constraint_accepts_scalars_and_pointers() {
    // I64, F64, and a pointer element all satisfy `comparable`.
    prog(
        "T Max<comparable T>(T a, T b) { return a > b ? a : b; } \
         U0 M() { Max(1, 2); Max(1.0, 2.0); U8 *p; U8 *q; Max(p, q); }",
    );
}

#[test]
fn value_param_call_requires_explicit_args() {
    // A function with a value parameter can't be called by inference.
    let err = parse("I64 Z<int N>() { return N; } U0 M() { Z(); }").unwrap_err();
    assert!(err.to_string().contains("value parameter"), "got: {err}");
}

#[test]
fn generic_arity_mismatch_is_an_error() {
    let err = parse("class Pair<type K, type V> { K k; V v; } Pair<I64> p;").unwrap_err();
    assert!(err.to_string().contains("expects 2 argument"), "got: {err}");
}

// ---- generic functions (monomorphization) ----

#[test]
fn generic_function_monomorphizes_by_type_arg() {
    // `T Id<type T>(T x){...}` plus calls `Id<I64>(..)` and `Id<F64>(..)` generate the
    // concrete `2IdI64` and `2IdF64`. The call sites resolve to those mangled names.
    let p = prog("T Id<type T>(T x) { return x; } I64 a = Id<I64>(1); F64 b = Id<F64>(2.0);");
    let funcs: Vec<&FuncDef> = p
        .items
        .iter()
        .filter_map(|s| match &s.kind {
            StmtKind::Func(f) => Some(f),
            _ => None,
        })
        .collect();
    assert!(funcs.iter().any(|f| f.name == "2IdI64"));
    assert!(funcs.iter().any(|f| f.name == "2IdF64"));
    assert!(!funcs.iter().any(|f| f.name == "Id")); // template emits no function
    // the I64 instance has a concrete I64 parameter and return type
    let id_i64 = funcs.iter().find(|f| f.name == "2IdI64").unwrap();
    assert_eq!(id_i64.ret, Type::I64);
    assert_eq!(id_i64.params[0].ty, Type::I64);
}

#[test]
fn generic_function_dedups() {
    let p = prog("T Id<type T>(T x){return x;} I64 a=Id<I64>(1); I64 b=Id<I64>(2);");
    let n = p
        .items
        .iter()
        .filter(|s| matches!(&s.kind, StmtKind::Func(f) if f.name == "2IdI64"))
        .count();
    assert_eq!(n, 1);
}

#[test]
fn generic_param_requires_a_kind_keyword() {
    // A generic parameter must be declared with `type` (or `comparable`/`int`). A bare
    // `<T>` is rejected in both class and function definitions; the explicit form works
    // (including a `public` generic fn whose return type is a bare type parameter).
    for src in [
        "class Box<T> { T v; }",
        "T Id<T>(T x) { return x; }",
        "public T Id<T>(T x) { return x; }",
    ] {
        let err = parse(src).expect_err("bare <T> must be rejected");
        assert!(
            err.to_string().contains("type")
                && err.to_string().to_lowercase().contains("generic parameter"),
            "expected a generic-parameter error for `{src}`, got: {err}",
        );
    }
    // The explicit `<type T>` form parses and monomorphizes.
    let p = prog(
        "public class Box<type T> { T v; }
         public T BoxGet<type T>(Box<T> *b) { return b->v; }
         U0 Main() { Box<I64> b; b.v = 5; BoxGet<I64>(&b); }",
    );
    let has_instance = p.items.iter().any(|s| match &s.kind {
        StmtKind::Func(f) => f.name.contains("BoxGet"),
        _ => false,
    });
    assert!(has_instance, "BoxGet<I64> instance should exist");
}

#[test]
fn generic_function_infers_type_args() {
    // `Id<type T>(T)` called as `Id(1)` infers `T=I64`. That generates `2IdI64` and
    // resolves the un-annotated call to it.
    let p = prog("T Id<type T>(T x){return x;} I64 a = Id(1);");
    let funcs: Vec<&FuncDef> = p
        .items
        .iter()
        .filter_map(|s| match &s.kind {
            StmtKind::Func(f) => Some(f),
            _ => None,
        })
        .collect();
    assert!(funcs.iter().any(|f| f.name == "2IdI64"));
}

#[test]
fn generic_function_infers_from_cast_and_call_result() {
    // `arg_type` now types both an explicit cast (from its own type) and a call
    // result (from the callee's recorded return type). Neither argument needs an
    // explicit `<...>`.
    let p = prog(
        "T Id<type T>(T x){return x;} I64 Mk(){return 7;} F64 MkF(){return 2.5;} \
         I64 a = Id(Mk()); F64 b = Id(MkF()); F64 c = Id((F64)3);",
    );
    let names: Vec<&str> = p
        .items
        .iter()
        .filter_map(|s| match &s.kind {
            StmtKind::Func(f) => Some(f.name.as_str()),
            _ => None,
        })
        .collect();
    assert!(names.contains(&"2IdI64"), "call-result I64: {names:?}");
    assert!(
        names.contains(&"2IdF64"),
        "call-result F64 + cast F64: {names:?}"
    );
}

#[test]
fn generic_inference_resolves_member_index_deref_and_arithmetic() {
    // The `mono` pass's typer handles member access, indexing, deref, and
    // arithmetic, using the resolved class-field types. None of these need an
    // explicit `<...>`. Each generic `T` here is inferable only from the
    // argument, with no receiver to recover it from, so a missing case would be
    // a hard "cannot infer" error.
    let names = |src: &str| -> Vec<String> {
        prog(src)
            .items
            .iter()
            .filter_map(|s| match &s.kind {
                StmtKind::Func(f) => Some(f.name.clone()),
                _ => None,
            })
            .collect()
    };
    // member access (I64 field) and a pointer-to-class arrow access
    let n = names(
        "class Box{I64 v; F64 f;} T Id<type T>(T x){return x;} \
         Box b; I64 a = Id(b.v); Box *p = &b; I64 c = Id(p->v);",
    );
    assert!(n.contains(&"2IdI64".to_string()), "member access: {n:?}");
    // member access of an F64 field binds T=F64
    let n = names("class Box{F64 f;} T Id<type T>(T x){return x;} Box b; F64 a = Id(b.f);");
    assert!(n.contains(&"2IdF64".to_string()), "F64 member: {n:?}");
    // indexing, deref, and arithmetic
    let n = names(
        "T Id<type T>(T x){return x;} I64 arr[3]; I64 a = Id(arr[0]); \
         I64 q = 1; I64 *pp = &q; I64 b = Id(*pp); I64 c = Id(q + a);",
    );
    assert!(
        n.contains(&"2IdI64".to_string()),
        "index/deref/arith: {n:?}"
    );
}

#[test]
fn generic_inference_sees_a_forward_declared_function() {
    // The `mono` pass types the whole parsed program. So a call-result argument
    // whose function is defined below the call site infers fine, with no special
    // hoisting needed.
    let p =
        parse("T Id<type T>(T x){return x;} I64 a = Id(Later()); I64 Later(){return 4;}").unwrap();
    assert!(
        p.items
            .iter()
            .any(|s| matches!(&s.kind, StmtKind::Func(f) if f.name == "2IdI64")),
        "Id<I64> should be instantiated from the forward-declared Later's return type"
    );
}

#[test]
fn mono_inference_closes_ternary_inherited_and_fnptr_gaps() {
    // The `mono` pass's whole-program, scoped typer infers cases the old parse-time
    // "seen so far" typer could not: a ternary argument, an inherited (base-class)
    // field, and a function-pointer call result.
    let names = |src: &str| -> Vec<String> {
        prog(src)
            .items
            .iter()
            .filter_map(|s| match &s.kind {
                StmtKind::Func(f) => Some(f.name.clone()),
                _ => None,
            })
            .collect()
    };
    // a ternary argument
    let n = names("T Id<type T>(T x){return x;} U0 F(){ I64 c=1; I64 a = Id(c ? 1 : 2); }");
    assert!(n.contains(&"2IdI64".to_string()), "ternary: {n:?}");
    // an inherited (base-class) field
    let n = names(
        "class Base{I64 v;} class Derived:Base{I64 w;} T Id<type T>(T x){return x;} \
         U0 F(){ Derived d; I64 a = Id(d.v); }",
    );
    assert!(n.contains(&"2IdI64".to_string()), "inherited field: {n:?}");
    // a function-pointer call result
    let n = names(
        "I64 G(I64 x){return x;} T Id<type T>(T x){return x;} \
         U0 F(){ I64 (*fp)(I64) = &G; I64 a = Id(fp(3)); }",
    );
    assert!(n.contains(&"2IdI64".to_string()), "fnptr result: {n:?}");
}

#[test]
fn mono_consumes_templates_and_emits_concrete_instances() {
    // After parse and the `mono` pass, the program is fully concrete. The generic
    // templates are consumed: they no longer appear on `Program::generics`. The
    // instances appear as ordinary top-level definitions — `Vec<I64>`
    // instantiated, and `Id(1)` inferred then instantiated.
    let p = parse(
        "class Vec<type T> { T *data; } T Id<type T>(T x) { return x; } Vec<I64> v; I64 a = Id(1);",
    )
    .unwrap();
    assert!(p.generics.classes.is_empty(), "templates consumed by mono");
    assert!(p.generics.fns.is_empty(), "templates consumed by mono");
    let has_class = |n: &str| {
        p.items
            .iter()
            .any(|s| matches!(&s.kind, StmtKind::Class(c) if c.name == n))
    };
    let has_fn = |n: &str| {
        p.items
            .iter()
            .any(|s| matches!(&s.kind, StmtKind::Func(f) if f.name == n))
    };
    assert!(has_class("3VecI64"), "Vec<I64> instantiated");
    assert!(has_fn("2IdI64"), "Id(1) inferred + instantiated");
}

#[test]
fn colon_eq_tuple_unpack() {
    // `a, b := e;` is the sole unpack syntax. It declares each name with the
    // element type inferred from the tuple (`_` discards a slot). It desugars to
    // a hidden tuple temp plus one declarator per named slot.
    let unpack_decls = |src: &str| -> Vec<Declarator> {
        prog(src)
            .items
            .iter()
            .find_map(|s| match &s.kind {
                StmtKind::VarDecl { decls } => Some(decls.clone()),
                _ => None,
            })
            .expect("a VarDecl from the unpack")
    };

    // From a tuple-returning function: 2 names + the temp = 3 declarators, typed I64.
    let d = unpack_decls("(I64, I64) Mk() { return 1, 2; } a, b := Mk();");
    assert_eq!(d.len(), 3, "temp + a + b: {d:?}");
    assert_eq!(d[1].name, "a");
    assert_eq!(d[1].ty, Type::I64);
    assert_eq!(d[2].name, "b");

    // From a tuple literal, element types inferred per element (I64, F64).
    let d = unpack_decls("x, y := (10, 2.5);");
    assert_eq!(d[1].ty, Type::I64);
    assert_eq!(d[2].ty, Type::F64);

    // `_` discards a slot (no declarator emitted for it): temp + only `b`.
    let d = unpack_decls("(I64, I64) Mk() { return 1, 2; } _, b := Mk();");
    assert_eq!(d.len(), 2, "temp + b only: {d:?}");
    assert_eq!(d[1].name, "b");
}

#[test]
fn colon_eq_single_infers_type() {
    // `n := e;` with ONE name is an inferred-type declaration. It produces one
    // declarator whose type comes from the right-hand side, with no tuple temp.
    // The helper finds the declarator named `want`.
    let decl = |src: &str, want: &str| -> Declarator {
        prog(src)
            .items
            .iter()
            .filter_map(|s| match &s.kind {
                StmtKind::VarDecl { decls } => Some(decls),
                _ => None,
            })
            .flatten()
            .find(|d| d.name == want)
            .unwrap_or_else(|| panic!("no declarator `{want}`"))
            .clone()
    };
    assert_eq!(decl("n := 5;", "n").ty, Type::I64);
    assert_eq!(decl("f := 2.5;", "f").ty, Type::F64);
    assert_eq!(decl("s := \"hi\";", "s").ty, Type::Ptr(Box::new(Type::U8)));
    // A whole tuple bound to one variable (its type is the tuple type, not unpacked).
    let d = decl("(I64, I64) Mk(){return 1,2;} p := Mk();", "p");
    assert!(matches!(d.ty, Type::Named(_)), "tuple type: {:?}", d.ty);
    // A pointer-returning generic call infers its pointer return type.
    let d = decl(
        "T *Id<type T>(T *x){return x;} I64 n = 0; p := Id<I64>(&n);",
        "p",
    );
    assert_eq!(
        d.ty,
        Type::Ptr(Box::new(Type::I64)),
        "pointer return: {:?}",
        d.ty
    );
}

#[test]
fn colon_eq_through_generic_calls() {
    // A generic call's tuple return is inferred for `:=`, in both the inferred and
    // explicit type-arg forms. The instance's return type is recorded at the call
    // site.
    let unpacks = |src: &str| -> bool {
        parse(src).is_ok_and(|p| {
            p.items
                .iter()
                .any(|s| matches!(&s.kind, StmtKind::Func(f) if f.name.contains("Pick")))
        })
    };
    let prog = "(T, Bool) Pick<type T>(T x) { return x, 1; } ";
    // inferred type arg
    assert!(
        unpacks(&format!("{prog} U0 F(){{ v, ok := Pick(5); }}")),
        "inferred"
    );
    // explicit type arg
    assert!(
        unpacks(&format!("{prog} U0 F(){{ v, ok := Pick<I64>(5); }}")),
        "explicit"
    );
}

#[test]
fn colon_eq_errors() {
    // Arity mismatch is a clear error.
    let err = parse("(I64, I64, I64) Mk(){return 1,2,3;} a, b := Mk();").unwrap_err();
    assert!(err.to_string().contains("3 element"), "got: {err}");

    // A non-tuple RHS can't be unpacked.
    let err = parse("U0 F(){ a, b := 5; }").unwrap_err();
    assert!(err.to_string().contains("cannot infer"), "got: {err}");

    // A single name whose RHS type can't be determined (`*` of a non-pointer) errors.
    let err = parse("U0 F(){ a := *5; }").unwrap_err();
    assert!(err.to_string().contains("cannot infer"), "got: {err}");

    // `I64 a, b = 5;` is still an ordinary declaration list, not an unpack.
    match stmt("I64 a, b = 5;") {
        StmtKind::VarDecl { decls } => assert_eq!(decls.len(), 2, "plain decl list"),
        other => panic!("expected a decl list, got {other:?}"),
    }
}
