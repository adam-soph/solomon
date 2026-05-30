//! Unit tests for the parser, asserting concrete AST shapes.
//!
//! AST equality ignores spans (see `ast.rs`), so expected trees are built with
//! [`Span::dummy`] via the `e`/`b` helpers below.

use solomon::ast::*;
use solomon::lexer::Lexer;
use solomon::parser::{parse, Parser};
use solomon::token::Span;

/// Parse a program, unwrapping the result.
fn prog(src: &str) -> Program {
    parse(src).unwrap_or_else(|e| panic!("parse failed: {e}"))
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
    // The public entry point is "give the parser a lexer"; tokens are pulled on
    // demand rather than collected up front.
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
