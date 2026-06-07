#ifndef _BITS_HC
#define _BITS_HC
// bits.hc — IEEE-754 double bit access, classification, and special values.
//
// This is the lowest layer of the math library: pure bit manipulation of an `F64`,
// with no dependency on the rest. `<math.hc>` includes it for `Fabs` and the
// functions that special-case Inf/NaN.

// Pun a double to/from its 64-bit pattern. solomon lays a union out as raw bytes, so
// this works identically on the interpreter and every backend.
// Private to the stdlib directory: user code puns via `Float64bits`/`Float64frombits`.
union F64Bits { F64 f; U64 u; }

public I64 Float64bits(F64 x)     { F64Bits v; v.f = x; return v.u; }
public F64 Float64frombits(I64 b) { F64Bits v; v.u = b; return v.f; }

public F64 NaN()                  { F64Bits v; v.u = 0x7FF8000000000000; return v.f; }
public F64 Inf(I64 sign)          { F64Bits v; if (sign >= 0) v.u = 0x7FF0000000000000; else v.u = 0xFFF0000000000000; return v.f; }

public I64 IsNaN(F64 x)           { return x != x; }
public I64 Signbit(F64 x)         { F64Bits v; v.f = x; return (v.u >> 63) & 1; }

// `sign>0` tests only +Inf, `sign<0` only -Inf, `sign==0` either.
public I64 IsInf(F64 x, I64 sign)
{
  I64 pos = x > 1.7976931348623157e308;
  I64 neg = x < -1.7976931348623157e308;
  if (sign > 0) return pos;
  if (sign < 0) return neg;
  return pos || neg;
}

// Magnitude of `f` with the sign bit of `sign`.
public F64 Copysign(F64 f, F64 sign)
{
  F64Bits vf, vs;
  vf.f = f;
  vs.f = sign;
  vf.u = (vf.u & 0x7FFFFFFFFFFFFFFF) | (vs.u & 0x8000000000000000);
  return vf.f;
}

#endif
