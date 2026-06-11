//! Unit tests for the scalar-simplification pass. The end-to-end guarantee
//! (`native == interp == golden`) is enforced by the integration suite; these check the
//! self-contained arithmetic that must match the oracle bit-for-bit.

use super::*;

#[test]
fn pow2_recognizes_only_clean_powers() {
    assert_eq!(pow2(2), Some(1));
    assert_eq!(pow2(4), Some(2));
    assert_eq!(pow2(8), Some(3));
    assert_eq!(pow2(1i64 << 62), Some(62));
    // Not powers of two, or excluded edges.
    assert_eq!(pow2(1), None); // 2^0 — an identity, handled separately
    assert_eq!(pow2(3), None);
    assert_eq!(pow2(0), None);
    assert_eq!(pow2(-2), None);
    assert_eq!(pow2(1i64 << 63), None); // i64::MIN, negative
    assert_eq!(pow2(i64::MAX), None);
}

#[test]
fn unsigned_div_mod_pow2_match() {
    for k in 1..=62i64 {
        let d = 1i64 << k;
        for &x in &[0i64, 1, 7, d - 1, d, d + 1, i64::MAX, -1, i64::MIN] {
            let u = x as u64;
            assert_eq!(
                ((u >> k) as i64),
                (u / d as u64) as i64,
                "u/2^k x={x} k={k}"
            );
            assert_eq!((x & (d - 1)), (u % d as u64) as i64, "u%2^k x={x} k={k}");
        }
    }
}

#[test]
fn rval_roundtrip() {
    assert!(matches!(as_rval(Val::ImmInt(42)), Some(RVal::Int(42))));
    assert!(matches!(as_rval(Val::Reg(3)), None));
    match as_rval(Val::ImmF64(1.5f64.to_bits())) {
        Some(RVal::Float(f)) => assert_eq!(f, 1.5),
        _ => panic!("expected float"),
    }
    assert_eq!(from_rval(RVal::Int(9), IrTy::I64), Val::ImmInt(9));
    assert_eq!(
        from_rval(RVal::Float(2.0), IrTy::F64),
        Val::ImmF64(2.0f64.to_bits())
    );
}
