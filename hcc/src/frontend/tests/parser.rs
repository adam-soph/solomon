//! Unit tests for parser desugars and the operator-precedence table. Expressions
//! are parsed directly through the private `parse_expr`, and the chained-comparison
//! desugar is checked structurally. Reaches the module's private free helpers
//! (`is_chain_cmp`, `infix_op`) via `use super::*`.

use super::*;

/// Parse `src` as a single expression (bare lexer — no preprocessor or prelude).
fn expr(src: &str) -> Expr {
    let mut p = Parser::new(Lexer::new(src));
    p.parse_expr().expect("parse expr")
}

#[test]
fn chained_relational_desugars_to_and() {
    // a < b < c  ==>  (a < b) && (b < c)
    match expr("a < b < c").kind {
        ExprKind::Binary {
            op: BinOp::And,
            lhs,
            rhs,
        } => {
            assert!(matches!(lhs.kind, ExprKind::Binary { op: BinOp::Lt, .. }));
            assert!(matches!(rhs.kind, ExprKind::Binary { op: BinOp::Lt, .. }));
        }
        other => panic!("expected And of two Lt, got {other:?}"),
    }
}

#[test]
fn three_way_chain_ands_each_link() {
    // 0 <= i < n <= m  ==>  ((0 <= i) && (i < n)) && (n <= m)
    match expr("0 <= i < n <= m").kind {
        ExprKind::Binary { op: BinOp::And, .. } => {}
        other => panic!("expected top-level And, got {other:?}"),
    }
}

#[test]
fn equality_is_not_chained() {
    // a == b == c keeps the C meaning: (a == b) == c — left-associative, no desugar.
    match expr("a == b == c").kind {
        ExprKind::Binary {
            op: BinOp::Eq,
            lhs,
            rhs,
        } => {
            assert!(matches!(lhs.kind, ExprKind::Binary { op: BinOp::Eq, .. }));
            assert!(matches!(rhs.kind, ExprKind::Ident(_)));
        }
        other => panic!("expected left-assoc Eq, got {other:?}"),
    }
}

#[test]
fn precedence_mul_binds_tighter_than_add() {
    // 1 + 2 * 3  ==>  1 + (2 * 3)
    match expr("1 + 2 * 3").kind {
        ExprKind::Binary {
            op: BinOp::Add,
            lhs,
            rhs,
        } => {
            assert!(matches!(lhs.kind, ExprKind::Int(1)));
            assert!(matches!(rhs.kind, ExprKind::Binary { op: BinOp::Mul, .. }));
        }
        other => panic!("expected Add, got {other:?}"),
    }
}

// ---- free helpers ----

#[test]
fn is_chain_cmp_only_relational() {
    assert!(is_chain_cmp(BinOp::Lt));
    assert!(is_chain_cmp(BinOp::Gt));
    assert!(is_chain_cmp(BinOp::Le));
    assert!(is_chain_cmp(BinOp::Ge));
    assert!(!is_chain_cmp(BinOp::Eq));
    assert!(!is_chain_cmp(BinOp::Ne));
    assert!(!is_chain_cmp(BinOp::Add));
}

#[test]
fn infix_op_binding_powers_are_ordered() {
    // logical-or < logical-and < relational < shift < additive < multiplicative.
    let bp = |k: TokenKind| infix_op(&k).expect("infix op").1;
    assert!(bp(TokenKind::OrOr) < bp(TokenKind::AndAnd));
    assert!(bp(TokenKind::AndAnd) < bp(TokenKind::Lt));
    assert!(bp(TokenKind::Lt) < bp(TokenKind::Shl));
    assert!(bp(TokenKind::Shl) < bp(TokenKind::Plus));
    assert!(bp(TokenKind::Plus) < bp(TokenKind::Star));
    // '=' is assignment, handled separately — not an infix binary operator.
    assert_eq!(infix_op(&TokenKind::Eq), None);
}
