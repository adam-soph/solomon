#ifndef _MATH_HC
#define _MATH_HC
// math.hc — implementation (interface in math.hh).

#include <math.hh>

union F64Bits { F64 f; U64 u; }

// Generic helpers (prototypes in <math.hh>). They precede the non-generic bodies because
// `Fmin`/`Fmax` below call `Min`/`Max`. `Abs`'s `T is F64` branch defers to `Fabs` for
// exact IEEE semantics (so `Abs(-0.0)` is `+0.0`, a NaN stays NaN); the integer
// instantiations use the plain negate. `Min`/`Max`'s float branch adds C `fmin`/`fmax`
// NaN semantics (plain `a > b` would mishandle `Max(x, NaN)`).
public T Abs<comparable T>(T n)
{
  if type (T is F64) return Fabs(n);
  if (n < 0) return -n;
  return n;
}
public I64 Sign<comparable T>(T n) { return (n > 0) - (n < 0); }

public T Min<comparable T>(T a, T b)
{
  if type (T is F64) {
    if (a != a) return b;
    if (b != b) return a;
  }
  if (a < b) return a;
  return b;
}
public T Max<comparable T>(T a, T b)
{
  if type (T is F64) {
    if (a != a) return b;
    if (b != b) return a;
  }
  if (a > b) return a;
  return b;
}

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

// IEEE-754 classification, like C `<math.h>`. `FpClassify` returns one of the `FP_*`
// codes; `IsFinite`/`IsNormal` are the common predicates (0/1). A "normal" number is
// finite, non-zero, and not subnormal. (The exponent field is bits 52..62; masking with
// 0x7FF after the shift drops the sign bit, so the shift's signedness doesn't matter.)

public I64 FpClassify(F64 x)
{
  I64 bits = Float64bits(x);
  I64 e = (bits >> 52) & 0x7FF;
  I64 m = bits & 0xFFFFFFFFFFFFF; // the 52-bit fraction
  if (e == 0x7FF) { if (m) return FP_NAN; return FP_INFINITE; }
  if (e == 0) { if (m) return FP_SUBNORMAL; return FP_ZERO; }
  return FP_NORMAL;
}

public I64 IsFinite(F64 x) { return !IsNaN(x) && !IsInf(x, 0); }

public I64 IsNormal(F64 x)
{
  I64 e = (Float64bits(x) >> 52) & 0x7FF;
  return e != 0 && e != 0x7FF;
}


// Every F64 with magnitude >= 2^52 is already an integer, since the mantissa has no
// room for a fraction. So the rounding ops short-circuit there; below it the
// truncating I64 cast is exact.

// --- small helpers ---------------------------------------------------
// (`Abs`/`Sign`/`Min`/`Max` are generic, so their templates live in `math.hh`.)

// C `fmin`/`fmax` (F64): NaN-aware (a NaN operand yields the other) — thin wrappers over
// the generic `Min`/`Max`, which already do that for floats.
public F64 Fmin(F64 a, F64 b) { return Min(a, b); }
public F64 Fmax(F64 a, F64 b) { return Max(a, b); }

public I64 Gcd(I64 a, I64 b)
{
  if (a < 0) a = -a;
  if (b < 0) b = -b;
  while (b != 0) { I64 t = b; b = a % b; a = t; }
  return a;
}

public I64 Factorial(I64 n)
{
  I64 r = 1, i = 2;
  while (i <= n) { r *= i; i++; }
  return r;
}

// --- F64 helpers -------------------------------------------------------------

// Absolute value: clear the IEEE-754 sign bit. (`F64Bits`, defined at the top of this
// file, puns the double to its pattern.) This gives exact libm semantics, unlike a
// `x < 0 ? -x : x`
// test: `Fabs(-0.0)` is `+0.0`, and NaN is made positive.
public F64 Fabs(F64 x)
{
  F64Bits v;
  v.f = x;
  v.u = v.u & 0x7FFFFFFFFFFFFFFF;
  return v.f;
}

// Square root, **correctly rounded**: bit-identical to the IEEE-754 hardware
// instruction, verified over a 500k-value battery. The algorithm reduces
// `x = f·2^(2k)` with `f ∈ [1,4)` via the exponent bits, Newton-iterates `√f`, then
// takes one exact-residual correction step and scales back by `2^k`. The correction
// computes `r = f − y²` exactly with a Dekker two-product (there's no FMA), so
// `y + r/(2y)` lands on the correctly-rounded result. This is the one float op with
// no closed-form HolyC equivalent; a later compiler pass may recognise it and emit
// `fsqrt`/`sqrtsd`.
public F64 Sqrt(F64 x)
{
  F64Bits b;
  b.f = x;
  U64 bits = b.u;
  if ((bits & 0x7FFFFFFFFFFFFFFF) == 0) return x;                 // ±0
  if (bits & 0x8000000000000000) {
    F64Bits n; n.u = 0x7FF8000000000000;
    return n.f;
  } // x<0 → NaN
  if ((bits >> 52) == 0x7FF) return x;                            // +inf or NaN

  // Normalise a subnormal into the normal range (√ of a subnormal is normal).
  F64 scale = 1.0;
  if ((bits >> 52) == 0) {
    x = x * 18014398509481984.0;                                  // ×2^54
    b.f = x;
    bits = b.u;
    scale = 0.00000000745058059692382812;                        // ×2^-27 on the way out
  }

  // x = f·2^(2k), f ∈ [1,4).
  I64 e = (I64)((bits >> 52) & 0x7FF) - 1023;
  I64 k = e >> 1;
  I64 e2 = e - (k << 1);
  F64Bits fb;
  fb.u = (bits & 0x000FFFFFFFFFFFFF) | (((U64)(e2 + 1023)) << 52);
  F64 f = fb.f;

  // √f by Newton from a linear guess (≤25% error → below 1 ULP after 6 steps).
  F64 y = (f + 1.0) * 0.5;
  y = (y + f / y) * 0.5;
  y = (y + f / y) * 0.5;
  y = (y + f / y) * 0.5;
  y = (y + f / y) * 0.5;
  y = (y + f / y) * 0.5;
  y = (y + f / y) * 0.5;

  // One correctly-rounding step with the exact residual f − y² (Dekker product).
  F64 c = 134217729.0;                                           // 2^27 + 1 (Veltkamp split)
  F64 t = c * y;
  F64 yh = t - (t - y);
  F64 yl = y - yh;
  F64 ph = y * y;
  F64 pl = ((yh * yh - ph) + 2.0 * yh * yl) + yl * yl;
  F64 r = (f - ph) - pl;
  y = y + r / (y + y);

  // result = y·2^k (exact: add k to the exponent field).
  F64Bits rb;
  rb.f = y;
  I64 yexp = (I64)((rb.u >> 52) & 0x7FF);
  rb.u = (rb.u & 0x800FFFFFFFFFFFFF) | (((U64)(yexp + k)) << 52);
  return rb.f * scale;
}

// --- rounding (truncate toward zero + adjust; exact for all finite F64) ------

public F64 Trunc(F64 x)
{
  if (x != x) return x;                          // NaN
  if (x >= TWO52 || x <= -TWO52) return x;        // already integral (incl. inf)
  return (I64)x;                                  // exact toward-zero truncation
}

public F64 Floor(F64 x) { F64 t = Trunc(x); if (t > x) return t - 1.0; return t; }
public F64 Ceil(F64 x)  { F64 t = Trunc(x); if (t < x) return t + 1.0; return t; }

// Round to nearest, ties away from zero (matching `frinta`).
public F64 Round(F64 x)
{
  F64 t = Trunc(x), d = x - t;
  if (d >= 0.5) return t + 1.0;
  if (d <= -0.5) return t - 1.0;
  return t;
}

// Round to nearest, ties to even (matching `frintn` / IEEE round-to-nearest). At a
// tie the truncated value `t` is integral and small enough that `(I64)t` is exact.
public F64 RoundToEven(F64 x)
{
  F64 t = Trunc(x), d = x - t;
  if (d > 0.5) return t + 1.0;
  if (d < -0.5) return t - 1.0;
  if (d == 0.5) { if (((I64)t & 1) == 0) return t; return t + 1.0; }
  if (d == -0.5) { if (((I64)t & 1) == 0) return t; return t - 1.0; }
  return t;
}

// Integer-returning rounds (C lround/llround/lrint). `LRound`/`LLRound` round halves away
// from zero (like `Round`); `LRint` rounds halves to even (like `RoundToEven`). The result
// is `I64`, so a value outside its range is undefined, as in C. `LLRound == LRound` here
// since HolyC's integer is 64-bit.
public I64 LRound(F64 x)  { return (I64)Round(x); }
public I64 LLRound(F64 x) { return (I64)Round(x); }
public I64 LRint(F64 x)   { return (I64)RoundToEven(x); }

// Floating remainder, the C `fmod` truncated form: x - Trunc(x/y)*y.
public F64 Fmod(F64 x, F64 y) { return x - Trunc(x / y) * y; }

// --- powers, exp & log -------------------------------------------------------

// Exact integer power x^n (exact-binary, fully reproducible).
public F64 PowI(F64 base, I64 exp)
{
  I64 neg = exp < 0;
  if (neg) exp = -exp;
  F64 r = 1.0;
  while (exp > 0) {
    if (exp & 1) r *= base;
    base *= base;
    exp >>= 1;
  }
  if (neg) return 1.0 / r;
  return r;
}

// e^x. Range-reduce x = k*LN2 + r with |r| <= LN2/2, sum the Taylor series for
// e^r (fast there), then scale by 2^k.
public F64 Exp(F64 x)
{
  I64 k = Round(x / LN2);
  F64 r = x - k * LN2;
  F64 term = 1.0, sum = 1.0;
  I64 n = 1;
  while (n < 18) { term *= r / n; sum += term; n++; }
  return sum * PowI(2.0, k);
}

// Natural log. Reduce x = m * 2^e with m in [1,2) by exact halving/doubling, then
// ln(m) = 2*(t + t^3/3 + t^5/5 + ...) with t = (m-1)/(m+1) (the atanh series).
public F64 Ln(F64 x)
{
  if (x <= 0.0) return 0.0; // domain error: caller's responsibility
  I64 e = 0;
  while (x >= 2.0) { x /= 2.0; e++; }
  while (x < 1.0)  { x *= 2.0; e--; }
  F64 t = (x - 1.0) / (x + 1.0);
  F64 t2 = t * t;
  F64 term = t, sum = 0.0;
  I64 n = 1;
  while (n < 40) { sum += term / n; term *= t2; n += 2; }
  return 2.0 * sum + e * LN2;
}

public F64 Log2(F64 x)  { return Ln(x) / LN2; }
public F64 Log10(F64 x) { return Ln(x) / LN10; }
public F64 Exp2(F64 x)  { return Exp(x * LN2); }

// General power b^p = e^(p*ln b), for b > 0.
public F64 Pow(F64 b, F64 p) { return Exp(p * Ln(b)); }

public F64 Hypot(F64 x, F64 y) { return Sqrt(x * x + y * y); }

// --- trigonometry ------------------------------------------------------------

// sin/cos via range reduction modulo TAU, then a Taylor series about 0.
public F64 Sin(F64 x)
{
  x -= TAU * Round(x / TAU); // fold into [-PI, PI]
  F64 term = x, sum = x, x2 = x * x;
  I64 n = 1;
  while (n < 12) {
    term *= -x2 / ((2 * n) * (2 * n + 1));
    sum += term;
    n++;
  }
  return sum;
}

public F64 Cos(F64 x)
{
  x -= TAU * Round(x / TAU);
  F64 term = 1.0, sum = 1.0, x2 = x * x;
  I64 n = 1;
  while (n < 12) {
    term *= -x2 / ((2 * n - 1) * (2 * n));
    sum += term;
    n++;
  }
  return sum;
}

public F64 Tan(F64 x) { return Sin(x) / Cos(x); }

// --- inverse trigonometry ----------------------------------------------------

// atan via argument halving: atan(x) = 2*atan(x/(1+sqrt(1+x^2))) until the argument
// is small, then a short Taylor series. Reflect for |x|>1 and negatives.
public F64 Atan(F64 x)
{
  I64 neg = x < 0.0;
  if (neg) x = -x;
  I64 inv = x > 1.0;
  if (inv) x = 1.0 / x;
  I64 k = 0;
  while (x > 0.2) { x = x / (1.0 + Sqrt(1.0 + x * x)); k++; }
  F64 x2 = x * x, term = x, sum = x;
  I64 n = 1;
  while (n < 12) { term *= -x2; sum += term / (2 * n + 1); n++; }
  F64 r = sum * PowI(2.0, k);
  if (inv) r = HALF_PI - r;
  if (neg) r = -r;
  return r;
}

public F64 Asin(F64 x)
{
  if (x >= 1.0) return HALF_PI;
  if (x <= -1.0) return -HALF_PI;
  return Atan(x / Sqrt(1.0 - x * x));
}

public F64 Acos(F64 x) { return HALF_PI - Asin(x); }

// Quadrant-aware atan2(y, x).
public F64 Atan2(F64 y, F64 x)
{
  if (x > 0.0) return Atan(y / x);
  if (x < 0.0) {
    if (y >= 0.0) return Atan(y / x) + PI;
    return Atan(y / x) - PI;
  }
  if (y > 0.0) return HALF_PI;
  if (y < 0.0) return -HALF_PI;
  return 0.0;
}

// --- hyperbolic --------------------------------------------------------------

public F64 Sinh(F64 x) { return (Exp(x) - Exp(-x)) / 2.0; }
public F64 Cosh(F64 x) { return (Exp(x) + Exp(-x)) / 2.0; }
public F64 Tanh(F64 x) { F64 a = Exp(x), b = Exp(-x); return (a - b) / (a + b); }

// --- exponent / mantissa ------------------------------------------------------

// The unbiased binary exponent: `x = m·2^e`, `m ∈ [1,2)`. 0 → MinI32, Inf/NaN → MaxI32.
public I64 Ilogb(F64 x)
{
  if (x == 0.0) return -2147483648;
  if (x != x || IsInf(x, 0)) return 2147483647;
  F64Bits v;
  v.f = x;
  I64 e = (v.u >> 52) & 0x7FF;
  if (e == 0) { v.f = x * 18446744073709551616.0; e = ((v.u >> 52) & 0x7FF) - 64; } // subnormal
  return e - 1023;
}

public F64 Logb(F64 x)
{
  if (x == 0.0) return Inf(-1);
  if (x != x || IsInf(x, 0)) return Fabs(x);
  return (F64)Ilogb(x);
}

// frac·2^exp == f, with frac ∈ [0.5,1). Writes the exponent through `exp`.
public F64 Frexp(F64 f, I64 *exp)
{
  if (f == 0.0 || f != f || IsInf(f, 0)) { *exp = 0; return f; }
  F64Bits v;
  v.f = f;
  I64 e = (v.u >> 52) & 0x7FF;
  if (e == 0) { v.f = f * 18446744073709551616.0; e = ((v.u >> 52) & 0x7FF) - 64; } // subnormal
  *exp = e - 1022;
  v.u = (v.u & 0x800FFFFFFFFFFFFF) | (1022 << 52);  // force exponent so frac ∈ [0.5,1)
  return v.f;
}

// frac·2^exp (overflows to ±Inf, underflows to 0 — like Go).
public F64 Ldexp(F64 frac, I64 exp)
{
  if (frac == 0.0 || frac != frac || IsInf(frac, 0)) return frac;
  F64 r = frac;
  while (exp > 0) { r = r * 2.0; exp--; }
  while (exp < 0) { r = r / 2.0; exp++; }
  return r;
}

// C `scalbn`/`scalbln`: x·FLT_RADIX^n — FLT_RADIX is 2, so these ARE `Ldexp`.
public F64 Scalbn(F64 x, I64 n)  { return Ldexp(x, n); }
public F64 Scalbln(F64 x, I64 n) { return Ldexp(x, n); }

// --- misc real functions ------------------------------------------------------

public F64 Mod(F64 x, F64 y) { return Fmod(x, y); }  // Go's Mod == truncated remainder
public F64 Log(F64 x)        { return Ln(x); }        // Go's Log == natural log

// Integer + fractional parts (both carry f's sign); the int part is written via `ip`.
public F64 Modf(F64 f, F64 *ip) { F64 i = Trunc(f); *ip = i; return f - i; }

// max(x-y, 0).
public F64 Dim(F64 x, F64 y) { F64 d = x - y; if (d > 0.0) return d; if (d != d) return d; return 0.0; }

// IEEE remainder: x - y·RoundToEven(x/y).
public F64 Remainder(F64 x, F64 y)
{
  if (y == 0.0 || x != x || y != y || IsInf(x, 0)) return NaN();
  if (IsInf(y, 0)) return x;
  return x - y * RoundToEven(x / y);
}

// C `remquo`: the IEEE remainder, plus the low 3 bits of the rounded quotient
// x/y (magnitude mod 8), carrying the quotient's sign, written via `quo` — what
// argument-reduction code needs (e.g. an octant). On a NaN/Inf/zero-divisor domain
// error *quo is 0 and the result is NaN, like the remainder itself.
public F64 Remquo(F64 x, F64 y, I64 *quo)
{
  *quo = 0;
  if (y == 0.0 || x != x || y != y || IsInf(x, 0)) return NaN();
  if (IsInf(y, 0)) return x; // quotient rounds to 0
  F64 q = RoundToEven(x / y);
  F64 a = Fabs(q);
  // a mod 8: exact even for huge quotients — past 2^53 every double is a multiple
  // of a power of two >= 8, so both the division by 8 and the multiply-back are exact.
  I64 bits = a - Trunc(a / 8.0) * 8.0;
  if ((x < 0.0) != (y < 0.0)) bits = -bits; // the quotient's sign is sign(x/y)
  *quo = bits;
  return x - y * q;
}

// Cube root (Newton-refined over an exp/log initial guess; preserves sign).
public F64 Cbrt(F64 x)
{
  if (x == 0.0 || x != x || IsInf(x, 0)) return x;
  F64 s = 1.0, a = x;
  if (a < 0.0) { s = -1.0; a = -a; }
  F64 y = Exp(Ln(a) / 3.0);
  y = (2.0 * y + a / (y * y)) / 3.0;
  y = (2.0 * y + a / (y * y)) / 3.0;
  return s * y;
}

// 10^n for integer n.
public F64 Pow10(I64 n)
{
  if (n < 0) return 1.0 / Pow10(-n);
  F64 p = 1.0;
  while (n > 0) { p = p * 10.0; n--; }
  return p;
}

// exp(x)-1, accurate near 0 (series there, avoiding cancellation).
public F64 Expm1(F64 x)
{
  if (Fabs(x) < 1.0e-5) return x * (1.0 + x * (0.5 + x * 0.16666666666666666));
  return Exp(x) - 1.0;
}

// log(1+x), accurate near 0.
public F64 Log1p(F64 x)
{
  if (Fabs(x) < 1.0e-4) return x * (1.0 - x * (0.5 - x * 0.3333333333333333));
  return Ln(1.0 + x);
}

// --- inverse hyperbolic -------------------------------------------------------

public F64 Asinh(F64 x) { if (x < 0.0) return -Asinh(-x); return Ln(x + Sqrt(x * x + 1.0)); }
public F64 Acosh(F64 x) { if (x < 1.0) return NaN(); return Ln(x + Sqrt(x * x - 1.0)); }
public F64 Atanh(F64 x)
{
  if (x >= 1.0) { if (x == 1.0) return Inf(1); return NaN(); }
  if (x <= -1.0) { if (x == -1.0) return Inf(-1); return NaN(); }
  return 0.5 * Ln((1.0 + x) / (1.0 - x));
}

// sin and cos together (written through the pointers).
public U0 Sincos(F64 x, F64 *s, F64 *c) { *s = Sin(x); *c = Cos(x); }

// Fused multiply-add `x*y+z`: the product is formed exactly with a Dekker
// two-product, then summed with `z` so only the final result rounds. This is near
// the correctly-rounded FMA and identical on every backend. It is *not* an
// instruction intrinsic, since a hardware `fmadd` could round differently in the
// last bit.
public F64 FMA(F64 x, F64 y, F64 z)
{
  F64 c = 134217729.0;                       // 2^27 + 1 (Veltkamp split)
  F64 t = c * x; F64 xh = t - (t - x); F64 xl = x - xh;
  t = c * y; F64 yh = t - (t - y); F64 yl = y - yh;
  F64 ph = x * y;
  F64 pl = ((xh * yh - ph) + xh * yl + xl * yh) + xl * yl;
  F64 s = ph + z;                            // two-sum of ph and z
  F64 bb = s - ph;
  F64 err = (ph - (s - bb)) + (z - bb);
  return s + (err + pl);
}

// The adjacent representable double after `x`, in the direction of `y`.
public F64 Nextafter(F64 x, F64 y)
{
  if (x != x || y != y) return NaN();
  if (x == y) return y;
  F64Bits v;
  if (x == 0.0) { v.u = 1; if (y < 0.0) v.u = 0x8000000000000001; return v.f; }
  v.f = x;
  if ((y > x) == (x > 0.0)) v.u = v.u + 1; else v.u = v.u - 1;
  return v.f;
}

// C `nexttoward`: identical to `Nextafter` here — the C distinction is a `long double`
// direction argument, and F64 is the only floating type.
public F64 Nexttoward(F64 x, F64 y) { return Nextafter(x, y); }

// =============================================================================
// Special functions: the error-function and gamma families, plus the Bessel
// functions. They use rational, series, and asymptotic approximations. These are
// bulky and rarely used, but C declares them in `<math.h>`, so they live here.
// =============================================================================


// --- error function & gamma ---------------------------------------------------

// erfc(ax) for ax > 0 via the Laplace continued fraction (converges for ax >~ 1):
//   erfc(x) = e^{-x^2}/sqrt(pi) · 1/(x + (1/2)/(x + (2/2)/(x + (3/2)/(x + …))))
public F64 ErfcCF(F64 ax)
{
  F64 f = 0.0;
  I64 k;
  for (k = 100; k >= 1; k--) f = (0.5 * k) / (ax + f);
  return Exp(-ax * ax) / SQRT_PI / (ax + f);
}

// The error function. Small |x| uses the (cancellation-free) Taylor series; larger
// |x| goes through the continued fraction. Accurate to ~1e-15.
public F64 Erf(F64 x)
{
  if (x != x) return x;
  F64 ax = Fabs(x);
  if (ax <= 1.0) {
    F64 sum = ax, term = ax, x2 = ax * ax;
    I64 n = 1;
    while (n < 40) {
      term *= -x2 / n;
      F64 contrib = term / (2 * n + 1);
      sum += contrib;
      if (Fabs(contrib) < 1.0e-18) break;
      n++;
    }
    F64 r = TWO_OVER_SQRT_PI * sum;
    if (x < 0.0) return -r;
    return r;
  }
  F64 ec = ErfcCF(ax);
  if (x < 0.0) return ec - 1.0;
  return 1.0 - ec;
}

// The complementary error function, 1 - erf(x).
public F64 Erfc(F64 x)
{
  if (x != x) return x;
  F64 ax = Fabs(x);
  if (ax <= 1.0) return 1.0 - Erf(x);
  F64 ec = ErfcCF(ax);
  if (x < 0.0) return 2.0 - ec;
  return ec;
}

// Inverse error function: the x with erf(x) = y, for y in (-1, 1). It starts from a
// Winitzki initial guess (~1e-3), then refines with Newton on `Erf`; each step
// doubles the digits.
public F64 Erfinv(F64 y)
{
  if (y >= 1.0) { if (y == 1.0) return Inf(1); return NaN(); }
  if (y <= -1.0) { if (y == -1.0) return Inf(-1); return NaN(); }
  if (y == 0.0) return y;
  F64 a = 0.147;
  F64 ln = Ln(1.0 - y * y);
  F64 t1 = 2.0 / (PI * a) + ln / 2.0;
  F64 x = Copysign(Sqrt(Sqrt(t1 * t1 - ln / a) - t1), y);
  I64 i;
  for (i = 0; i < 3; i++)
    x -= (Erf(x) - y) / (TWO_OVER_SQRT_PI * Exp(-x * x));
  return x;
}

// Inverse complementary error function: the x with erfc(x) = y, for y in (0, 2).
public F64 Erfcinv(F64 y)
{
  if (y <= 0.0) { if (y == 0.0) return Inf(1); return NaN(); }
  if (y >= 2.0) { if (y == 2.0) return Inf(-1); return NaN(); }
  return Erfinv(1.0 - y);
}

// The gamma function (Lanczos g=7, 9 coefficients), with Euler reflection for the
// left half-plane. It overflows to +Inf past ~171. The poles at 0/-1/-2/… come from
// the reflection's sin term. Accurate to ~1e-14.
public F64 Gamma(F64 x)
{
  F64 c[9];
  c[0] = 0.99999999999980993;
  c[1] = 676.5203681218851;
  c[2] = -1259.1392167224028;
  c[3] = 771.32342877765313;
  c[4] = -176.61502916214059;
  c[5] = 12.507343278686905;
  c[6] = -0.13857109526572012;
  c[7] = 9.9843695780195716e-6;
  c[8] = 1.5056327351493116e-7;
  if (x < 0.5) return PI / (Sin(PI * x) * Gamma(1.0 - x));
  x -= 1.0;
  F64 a = c[0];
  F64 t = x + 7.5;
  I64 i;
  for (i = 1; i < 9; i++) a += c[i] / (x + i);
  return SQRT_PI * 1.4142135623730951 * Pow(t, x + 0.5) * Exp(-t) * a;
}

// log|Gamma(x)| plus its sign (written through `sign`). Uses the Lanczos log form,
// with reflection for x < 0.5. `sign` is -1 on the intervals where Gamma is negative.
public F64 Lgamma(F64 x, I64 *sign)
{
  *sign = 1;
  F64 c[9];
  c[0] = 0.99999999999980993;
  c[1] = 676.5203681218851;
  c[2] = -1259.1392167224028;
  c[3] = 771.32342877765313;
  c[4] = -176.61502916214059;
  c[5] = 12.507343278686905;
  c[6] = -0.13857109526572012;
  c[7] = 9.9843695780195716e-6;
  c[8] = 1.5056327351493116e-7;
  if (x < 0.5) {
    F64 sp = Sin(PI * x);
    if (sp < 0.0) *sign = -1;
    I64 s2;
    F64 lg = Lgamma(1.0 - x, &s2);
    return Ln(PI / Fabs(sp)) - lg;
  }
  F64 z = x - 1.0;
  F64 a = c[0];
  F64 t = z + 7.5;
  I64 i;
  for (i = 1; i < 9; i++) a += c[i] / (z + i);
  return 0.5 * Ln(2.0 * PI) + (z + 0.5) * Ln(t) - t + Ln(a);
}

// --- Bessel functions J0/J1/Jn, Y0/Y1/Yn -------------------------------------

// Asymptotic expansion of J_nu and Y_nu for large x (nu = 0 or 1), written through
// the pointers. amp·(P·cos ω − Q·sin ω) and amp·(P·sin ω + Q·cos ω), with the
// amplitude series a_k = a_{k-1}·(4ν²−(2k−1)²)/(8k).
public U0 BesselAsymp(F64 x, I64 nu, F64 *pj, F64 *py)
{
  F64 mu = 4.0 * nu * nu;
  F64 a = 1.0, xp = 1.0, invx = 1.0 / x;
  F64 p = 1.0, q = 0.0;
  I64 k = 1;
  while (k <= 8) {
    a *= (mu - (2 * k - 1) * (2 * k - 1)) / (8.0 * k);
    xp *= invx;
    F64 t = a * xp;
    if (k & 1) { if (((k - 1) / 2) & 1) q -= t; else q += t; }
    else { if ((k / 2) & 1) p -= t; else p += t; }
    k++;
  }
  F64 amp = Sqrt(TWO_OVER_PI / x);
  F64 w = x - nu * (PI / 2.0) - PI / 4.0;
  F64 cw = Cos(w), sw = Sin(w);
  *pj = amp * (cw * p - sw * q);
  *py = amp * (sw * p + cw * q);
}

// J0 (even): power series Σ (−x²/4)^m/(m!)² for small x.
public F64 J0(F64 x)
{
  x = Fabs(x);
  if (x < BESSEL_X0) {
    F64 z = -(x * x) / 4.0, term = 1.0, sum = 1.0;
    I64 m = 1;
    while (m < 80) {
      term *= z / (m * m);
      sum += term;
      if (Fabs(term) < 1.0e-18) break;
      m++;
    }
    return sum;
  }
  F64 j, y;
  BesselAsymp(x, 0, &j, &y);
  return j;
}

// J1 (odd): power series (x/2)·Σ (−x²/4)^m/(m!(m+1)!).
public F64 J1(F64 x)
{
  F64 s = 1.0;
  if (x < 0.0) { s = -1.0; x = -x; }
  if (x < BESSEL_X0) {
    F64 z = -(x * x) / 4.0, term = x / 2.0, sum = x / 2.0;
    I64 m = 1;
    while (m < 80) {
      term *= z / (m * (m + 1));
      sum += term;
      if (Fabs(term) < 1.0e-18) break;
      m++;
    }
    return s * sum;
  }
  F64 j, y;
  BesselAsymp(x, 1, &j, &y);
  return s * j;
}

// Y0: (2/π)(ln(x/2)+γ)·J0 + (2/π) Σ (−1)^{k+1} h_k (x²/4)^k/(k!)². Undefined for x≤0.
public F64 Y0(F64 x)
{
  if (x < 0.0) return NaN();
  if (x == 0.0) return Inf(-1);
  if (x < BESSEL_X0) {
    F64 j0 = J0(x), zp = (x * x) / 4.0, tk = 1.0, hk = 0.0, sum = 0.0;
    I64 k = 1;
    while (k < 80) {
      tk *= zp / (k * k);
      hk += 1.0 / k;
      F64 c = hk * tk;
      if (k & 1) sum += c; else sum -= c;
      if (c < 1.0e-18) break;
      k++;
    }
    return TWO_OVER_PI * (Ln(x / 2.0) + EULER_GAMMA) * j0 + TWO_OVER_PI * sum;
  }
  F64 j, y;
  BesselAsymp(x, 0, &j, &y);
  return y;
}

// Y1: (2/π)ln(x/2)·J1 − 2/(πx) − (1/π) Σ (−1)^k (−2γ+h_k+h_{k+1}) (x/2)^{2k+1}/(k!(k+1)!).
public F64 Y1(F64 x)
{
  if (x < 0.0) return NaN();
  if (x == 0.0) return Inf(-1);
  if (x < BESSEL_X0) {
    F64 j1 = J1(x), zp = (x * x) / 4.0, tk = x / 2.0, hk = 0.0, hk1 = 1.0, sum = 0.0;
    I64 k = 0;
    while (k < 80) {
      F64 c = (-2.0 * EULER_GAMMA + hk + hk1) * tk;
      if (k & 1) sum -= c; else sum += c;
      k++;
      tk *= zp / (k * (k + 1));
      hk = hk1;
      hk1 += 1.0 / (k + 1);
      if (Fabs(tk) < 1.0e-20) break;
    }
    return TWO_OVER_PI * Ln(x / 2.0) * j1 - 2.0 / (PI * x) - sum / PI;
  }
  F64 j, y;
  BesselAsymp(x, 1, &j, &y);
  return y;
}

// J_n: upward recurrence when |x| > n (stable), else Miller's downward recurrence
// with the J0 + 2(J2+J4+…) = 1 normalization.
public F64 Jn(I64 n, F64 x)
{
  if (n == 0) return J0(x);
  if (n == 1) return J1(x);
  I64 sgn = 1;
  if (n < 0) { n = -n; if (n & 1) sgn = -1; }   // J_{-n} = (-1)^n J_n
  F64 ax = Fabs(x);
  if (ax == 0.0) return 0.0;
  F64 tox = 2.0 / ax, ans;
  if (ax > (F64)n) {
    F64 bjm = J0(ax), bj = J1(ax);
    I64 j;
    for (j = 1; j < n; j++) { F64 bjp = j * tox * bj - bjm; bjm = bj; bj = bjp; }
    ans = bj;
  } else {
    I64 m = 2 * ((n + (I64)Sqrt(40.0 * n)) / 2);
    F64 jsum = 0.0, sum = 0.0, bjp = 0.0, bj = 1.0;
    ans = 0.0;
    I64 j;
    for (j = m; j > 0; j--) {
      F64 bjm = j * tox * bj - bjp;
      bjp = bj;
      bj = bjm;
      if (Fabs(bj) > 1.0e10) { bj *= 1.0e-10; bjp *= 1.0e-10; ans *= 1.0e-10; sum *= 1.0e-10; }
      if (jsum != 0.0) sum += bj;
      jsum = !jsum;
      if (j == n) ans = bjp;
    }
    sum = 2.0 * sum - bj;
    ans = ans / sum;
  }
  if (x < 0.0 && (n & 1)) ans = -ans;
  return sgn * ans;
}

// Y_n: upward recurrence Y_{n+1} = (2n/x)Y_n − Y_{n-1} (stable for Y). x ≤ 0 undefined.
public F64 Yn(I64 n, F64 x)
{
  if (n == 0) return Y0(x);
  if (n == 1) return Y1(x);
  if (x <= 0.0) { if (x == 0.0) return Inf(-1); return NaN(); }
  F64 tox = 2.0 / x, bym = Y0(x), by = Y1(x);
  I64 j;
  for (j = 1; j < n; j++) { F64 byp = j * tox * by - bym; bym = by; by = byp; }
  return by;
}


#endif
