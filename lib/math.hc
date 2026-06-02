// math.hc — the solomon standard math library.
//
// Pure HolyC built on the F64 arithmetic and the algebraic builtins (Sqrt, Floor,
// Fabs, Round). Every function has a *defined algorithm*, so it computes the same
// bits on the interpreter and on every native backend (IEEE-754 F64 ops are
// deterministic) — the reproducibility the transcendentals were excluded from the
// builtin set to preserve. Include it with `#include <math.hc>`.

#define PI    3.14159265358979311600
#define TAU   6.28318530717958623200
#define E     2.71828182845904509080
#define LN2   0.69314718055994530942
#define SQRT2 1.41421356237309514547

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

// Exact integer power x^n (no approximation — exact-binary, fully reproducible).
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
// e^r (which converges quickly there), then scale by 2^k.
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

// General power b^p = e^(p*ln b), for b > 0.
F64 Pow(F64 b, F64 p) { return Exp(p * Ln(b)); }

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
