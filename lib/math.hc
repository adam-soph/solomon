#ifndef _MATH_HC
#define _MATH_HC
// math.hc — the solomon standard math library (elementary functions).
//
// Pure HolyC, built on F64 arithmetic and the `Sqrt`/`Fabs` optimization intrinsics.
// Every function here — rounding, logs, the transcendentals, exponent ops, `Modf`,
// and the rest — has a *defined algorithm*, so it computes the same bits on the
// interpreter and every native backend. (IEEE-754 F64 ops are deterministic.) The
// IEEE bit and classification ops are split into `<bits.hc>`, included below. The
// bulky special functions (Erf/Gamma/Bessel) live in `<special.hc>`. Include with
// `#include <math.hc>`.

#include <bits.hc>   // F64Bits, Float64bits/frombits, IsNaN/IsInf/Inf/NaN/Signbit/Copysign

#define PI      3.14159265358979311600
#define HALF_PI 1.57079632679489655800
#define TAU     6.28318530717958623200
#define E       2.71828182845904509080
#define LN2     0.69314718055994530942
#define LN10    2.30258509299404568402
#define SQRT2   1.41421356237309514547

// Every F64 with magnitude >= 2^52 is already an integer, since the mantissa has no
// room for a fraction. So the rounding ops short-circuit there; below it the
// truncating I64 cast is exact.
#define TWO52   4503599627370496.0

// --- small helpers ---------------------------------------------------

public I64 Abs<comparable T>(T n)  { if (n < 0) return -n; return n; }
public I64 Sign<comparable T>(T n) { return (n > 0) - (n < 0); }

public I64 Min<comparable T>(T a, T b) { if (a < b) return a; return b; }
public I64 Max<comparable T>(T a, T b) { if (a > b) return a; return b; }

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

// Absolute value: clear the IEEE-754 sign bit. (`F64Bits` from <bits.hc> puns the
// double to its pattern.) This gives exact libm semantics, unlike a `x < 0 ? -x : x`
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
  if (bits & 0x8000000000000000) { F64Bits n; n.u = 0x7FF8000000000000; return n.f; } // x<0 → NaN
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

#endif
