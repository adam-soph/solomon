#ifndef _BITS_HC
#define _BITS_HC
// bits.hc — IEEE-754 double bit access, classification, and special values.
//
// The lowest layer of the math library: pure bit manipulation of an `F64`, with no
// dependency on the rest. `<math.hc>` includes this for `Fabs` and the functions
// that special-case Inf/NaN. (Go's `Float32*`/`Nextafter32` are omitted — solomon
// has no `F32` type.) Include with `#include <bits.hc>`.

// Pun a double to/from its 64-bit pattern. solomon lays a union out as raw bytes, so
// this works identically on the interpreter and every backend.
union __F64Bits { F64 f; U64 u; }

I64 Float64bits(F64 x)     { __F64Bits v; v.f = x; return v.u; }
F64 Float64frombits(I64 b) { __F64Bits v; v.u = b; return v.f; }

F64 NaN()                  { __F64Bits v; v.u = 0x7FF8000000000000; return v.f; }
F64 Inf(I64 sign)          { __F64Bits v; if (sign >= 0) v.u = 0x7FF0000000000000; else v.u = 0xFFF0000000000000; return v.f; }

I64 IsNaN(F64 x)           { return x != x; }
I64 Signbit(F64 x)         { __F64Bits v; v.f = x; return (v.u >> 63) & 1; }

// `sign>0` tests only +Inf, `sign<0` only -Inf, `sign==0` either.
I64 IsInf(F64 x, I64 sign)
{
  I64 pos = x > 1.7976931348623157e308;
  I64 neg = x < -1.7976931348623157e308;
  if (sign > 0) return pos;
  if (sign < 0) return neg;
  return pos || neg;
}

// Magnitude of `f` with the sign bit of `sign`.
F64 Copysign(F64 f, F64 sign)
{
  __F64Bits vf, vs;
  vf.f = f;
  vs.f = sign;
  vf.u = (vf.u & 0x7FFFFFFFFFFFFFFF) | (vs.u & 0x8000000000000000);
  return vf.f;
}

#endif
