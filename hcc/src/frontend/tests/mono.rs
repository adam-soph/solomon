//! Unit tests for monomorphization: the pure parameter-substitution core
//! (`binds_of`, `subst_type`, `subst_arg`, `tuple_field`, `type_name`, the
//! `mangle_generic` naming) reached directly via `use super::*`, plus end-to-end
//! `expand` over a raw (un-monomorphized) program parsed with `parse_program`.

use super::*;

/// A dummy-spanned integer literal expression.
fn int(v: i64) -> Expr {
    Expr::new(ExprKind::Int(v), Span::dummy())
}

// ---- the substitution core (private free helpers) ----

#[test]
fn binds_of_pairs_params_with_args() {
    let params = vec![
        GenericParam::Type("T".into(), None),
        GenericParam::Value("N".into()),
    ];
    let args = vec![
        GenericArg::Type(Type::I64),
        GenericArg::Value(Box::new(int(8))),
    ];
    let binds = binds_of(&params, &args);
    assert!(matches!(binds.get("T"), Some(Bind::Type(Type::I64))));
    assert!(matches!(binds.get("N"), Some(Bind::Value(8))));
}

#[test]
fn subst_type_replaces_bound_param_only() {
    let binds = HashMap::from([("T".to_string(), Bind::Type(Type::I64))]);
    assert_eq!(subst_type(&Type::Param("T".into()), &binds), Type::I64);
    // An unbound parameter is left intact.
    assert_eq!(
        subst_type(&Type::Param("U".into()), &binds),
        Type::Param("U".into()),
    );
    // Pointers recurse into their pointee.
    assert_eq!(
        subst_type(&Type::Ptr(Box::new(Type::Param("T".into()))), &binds),
        Type::Ptr(Box::new(Type::I64)),
    );
}

#[test]
fn subst_type_substitutes_value_param_dimension() {
    // `T data[N]` with T = I8, N = 4  ==>  `I8 data[4]` (the dim Ident folds to Int).
    let binds = HashMap::from([
        ("T".to_string(), Bind::Type(Type::I8)),
        ("N".to_string(), Bind::Value(4)),
    ]);
    let arr = Type::Array(
        Box::new(Type::Param("T".into())),
        Some(Box::new(Expr::new(
            ExprKind::Ident("N".into()),
            Span::dummy(),
        ))),
    );
    match subst_type(&arr, &binds) {
        Type::Array(inner, Some(dim)) => {
            assert_eq!(*inner, Type::I8);
            assert!(matches!(dim.kind, ExprKind::Int(4)));
        }
        other => panic!("expected sized array, got {other:?}"),
    }
}

#[test]
fn subst_type_recurses_into_generic_args() {
    // `Vec<T>` with T = I64  ==>  `Vec<I64>` (still a deferred Generic, args resolved).
    let binds = HashMap::from([("T".to_string(), Bind::Type(Type::I64))]);
    let g = Type::Generic(
        "Vec".into(),
        vec![GenericArg::Type(Type::Param("T".into()))],
    );
    match subst_type(&g, &binds) {
        Type::Generic(name, args) => {
            assert_eq!(name, "Vec");
            assert_eq!(args.len(), 1);
            assert!(matches!(&args[0], GenericArg::Type(Type::I64)));
        }
        other => panic!("expected Generic, got {other:?}"),
    }
}

#[test]
fn subst_arg_folds_value_argument() {
    let binds = HashMap::from([("N".to_string(), Bind::Value(4))]);
    let arg = GenericArg::Value(Box::new(Expr::new(
        ExprKind::Ident("N".into()),
        Span::dummy(),
    )));
    match subst_arg(&arg, &binds) {
        GenericArg::Value(e) => assert!(matches!(e.kind, ExprKind::Int(4))),
        GenericArg::Type(_) => panic!("expected a Value arg"),
    }
}

#[test]
fn type_name_renders_pointers_and_arrays() {
    assert_eq!(type_name(&Type::Named("Foo".into())), "Foo");
    assert_eq!(
        type_name(&Type::Ptr(Box::new(Type::Named("Foo".into())))),
        "Foo *",
    );
    assert_eq!(
        type_name(&Type::Array(Box::new(Type::Named("Foo".into())), None)),
        "Foo[]",
    );
}

#[test]
fn tuple_field_builds_indexed_member_access() {
    // tuple_field("t", 2)  ==>  t._2
    match tuple_field("t", 2, Span::dummy()).kind {
        ExprKind::Member { base, field, arrow } => {
            assert!(matches!(base.kind, ExprKind::Ident(n) if n == "t"));
            assert_eq!(field, "_2");
            assert!(!arrow);
        }
        other => panic!("expected Member, got {other:?}"),
    }
}

#[test]
fn mangle_generic_is_deterministic_and_distinct() {
    let a = mangle_generic("Vec", &[GenericArg::Type(Type::I64)]);
    let b = mangle_generic("Vec", &[GenericArg::Type(Type::I64)]);
    let c = mangle_generic("Vec", &[GenericArg::Type(Type::F64)]);
    assert_eq!(a, b); // same args ⇒ same instance name (dedup relies on this)
    assert_ne!(a, c); // different args ⇒ different name
    assert!(a.contains("Vec"));
}

// ---- end-to-end expansion ----

/// Parse `src` WITHOUT monomorphization (raw `parse_program`), so deferred generic
/// nodes and the templates in `Program::generics` survive for `expand` to consume.
fn parse_raw(src: &str) -> Program {
    let mut p = crate::parser::Parser::new(crate::lexer::Lexer::new(src));
    p.parse_program().expect("parse_program")
}

/// The names of all top-level function definitions in `program`.
fn fn_names(program: &Program) -> Vec<String> {
    program
        .items
        .iter()
        .filter_map(|s| match &s.kind {
            StmtKind::Func(f) => Some(f.name.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn expand_instantiates_a_generic_function() {
    let prog = parse_raw("U0 Id<type T>(T x) { } U0 Main() { Id<I64>(5); }");
    let expanded = expand(prog).expect("expand");
    let mangled = mangle_generic("Id", &[GenericArg::Type(Type::I64)]);
    assert!(
        fn_names(&expanded).contains(&mangled),
        "expected instance `{mangled}` among {:?}",
        fn_names(&expanded),
    );
}

#[test]
fn expand_is_a_noop_without_generics() {
    let prog = parse_raw("U0 Main() { I64 x = 1; }");
    let before = fn_names(&prog);
    let expanded = expand(prog).expect("expand");
    assert_eq!(fn_names(&expanded), before);
}
