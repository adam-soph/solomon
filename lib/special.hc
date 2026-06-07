#ifndef _SPECIAL_HC
#define _SPECIAL_HC
// special.hc — the special functions: the error-function and gamma families, plus
// the Bessel functions. They use rational, series, and asymptotic approximations.
// These are bulky and rarely used, so they live apart from the elementary
// `<math.hc>` they build on. Include with `#include <special.hc>`.

#include <math.hc>

#define SQRT_PI          1.77245385090551602730   // sqrt(pi)
#define TWO_OVER_SQRT_PI 1.12837916709551257390   // 2/sqrt(pi)
#define EULER_GAMMA      0.57721566490153286061   // Euler-Mascheroni constant
#define TWO_OVER_PI      0.63661977236758134308   // 2/pi
#define BESSEL_X0        15.0                     // series-vs-asymptotic threshold

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
