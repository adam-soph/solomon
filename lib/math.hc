// math.hc — the solomon standard math library.
//
// Pure HolyC built on F64 arithmetic and the two irreducible algebraic builtins
// `Sqrt` (correctly-rounded — a hardware primitive) and `Fabs` (a sign-bit clear
// the interpreter models specially). Everything else — rounding, logs, the
// transcendentals — has a *defined algorithm* here, so it computes the same bits
// on the interpreter and every native backend (IEEE-754 F64 ops are
// deterministic). Include it with `#include <math.hc>`.

#define PI      3.14159265358979311600
#define HALF_PI 1.57079632679489655800
#define TAU     6.28318530717958623200
#define E       2.71828182845904509080
#define LN2     0.69314718055994530942
#define LN10    2.30258509299404568402
#define SQRT2   1.41421356237309514547

// Every F64 with magnitude >= 2^52 is already an integer (the mantissa has no
// room for a fraction), so the rounding ops short-circuit there; below it the
// truncating I64 cast is exact.
#define TWO52   4503599627370496.0

// --- small integer helpers ---------------------------------------------------

I64 IMin(I64 a, I64 b) { if (a < b) return a; return b; }
I64 IMax(I64 a, I64 b) { if (a > b) return a; return b; }

I64 Gcd(I64 a, I64 b)
{
  if (a < 0) a = -a;
  if (b < 0) b = -b;
  while (b != 0) { I64 t = b; b = a % b; a = t; }
  return a;
}

I64 Factorial(I64 n)
{
  I64 r = 1, i = 2;
  while (i <= n) { r *= i; i++; }
  return r;
}

// --- F64 helpers -------------------------------------------------------------

F64 FMin(F64 a, F64 b) { if (a < b) return a; return b; }
F64 FMax(F64 a, F64 b) { if (a > b) return a; return b; }
F64 Clamp(F64 x, F64 lo, F64 hi) { return FMax(lo, FMin(x, hi)); }

// --- rounding (truncate toward zero + adjust; exact for all finite F64) ------

F64 Trunc(F64 x)
{
  if (x != x) return x;                          // NaN
  if (x >= TWO52 || x <= -TWO52) return x;        // already integral (incl. inf)
  return (I64)x;                                  // exact toward-zero truncation
}

F64 Floor(F64 x) { F64 t = Trunc(x); if (t > x) return t - 1.0; return t; }
F64 Ceil(F64 x)  { F64 t = Trunc(x); if (t < x) return t + 1.0; return t; }

// Round to nearest, ties away from zero (matching the old builtin / `frinta`).
F64 Round(F64 x)
{
  F64 t = Trunc(x), d = x - t;
  if (d >= 0.5) return t + 1.0;
  if (d <= -0.5) return t - 1.0;
  return t;
}

// Floating remainder, the C `fmod` truncated form: x - Trunc(x/y)*y.
F64 Fmod(F64 x, F64 y) { return x - Trunc(x / y) * y; }

// --- powers, exp & log -------------------------------------------------------

// Exact integer power x^n (exact-binary, fully reproducible).
F64 PowI(F64 base, I64 exp)
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
F64 Exp(F64 x)
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
F64 Ln(F64 x)
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

F64 Log2(F64 x)  { return Ln(x) / LN2; }
F64 Log10(F64 x) { return Ln(x) / LN10; }
F64 Exp2(F64 x)  { return Exp(x * LN2); }

// General power b^p = e^(p*ln b), for b > 0.
F64 Pow(F64 b, F64 p) { return Exp(p * Ln(b)); }

F64 Hypot(F64 x, F64 y) { return Sqrt(x * x + y * y); }

// --- trigonometry ------------------------------------------------------------

// sin/cos via range reduction modulo TAU, then a Taylor series about 0.
F64 Sin(F64 x)
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

F64 Cos(F64 x)
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

F64 Tan(F64 x) { return Sin(x) / Cos(x); }

// --- inverse trigonometry ----------------------------------------------------

// atan via argument halving — atan(x) = 2*atan(x/(1+sqrt(1+x^2))) until the
// argument is small, then a short Taylor series; reflect for |x|>1 and negatives.
F64 Atan(F64 x)
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

F64 Asin(F64 x)
{
  if (x >= 1.0) return HALF_PI;
  if (x <= -1.0) return -HALF_PI;
  return Atan(x / Sqrt(1.0 - x * x));
}

F64 Acos(F64 x) { return HALF_PI - Asin(x); }

// Quadrant-aware atan2(y, x).
F64 Atan2(F64 y, F64 x)
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

F64 Sinh(F64 x) { return (Exp(x) - Exp(-x)) / 2.0; }
F64 Cosh(F64 x) { return (Exp(x) + Exp(-x)) / 2.0; }
F64 Tanh(F64 x) { F64 a = Exp(x), b = Exp(-x); return (a - b) / (a + b); }
