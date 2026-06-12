#ifndef _MATH_HH
#define _MATH_HH
// math.hh — the hcc standard math library (elementary functions).
//
// Pure HolyC, built on F64 arithmetic and the `Sqrt`/`Fabs` optimization intrinsics.
// Every function here — rounding, logs, the transcendentals, exponent ops, `Modf`,
// and the rest — has a *defined algorithm*, so it computes the same bits on the
// interpreter and every native backend. (IEEE-754 F64 ops are deterministic.) The IEEE
// bit access and classification (`isnan`/`isinf`/`signbit`/`copysign`/`nan`, C's
// `<math.h>` set) open the file, since the elementary functions special-case Inf/NaN on
// them. The special functions (the error-function, gamma, and Bessel families) are at the
// end of this file, also matching C's `<math.h>`. Include with `#include <math.hh>`.

// --- IEEE-754 bit access, classification & special values --------------------
//
// Pure bit manipulation of an `F64`. hcc lays a union out as raw bytes, so the
// punning works identically on the interpreter and every backend.
public I64 Float64bits(F64 x);
public F64 Float64frombits(I64 b);
public F64 NaN();
public F64 Inf(I64 sign);
public I64 IsNaN(F64 x);
public I64 Signbit(F64 x);
public I64 IsInf(F64 x, I64 sign);
public F64 Copysign(F64 f, F64 sign);
#define FP_NAN       0
#define FP_INFINITE  1
#define FP_ZERO      2
#define FP_SUBNORMAL 3
#define FP_NORMAL    4
public I64 FpClassify(F64 x);
public I64 IsFinite(F64 x);
public I64 IsNormal(F64 x);
#define PI      3.14159265358979311600
#define HALF_PI 1.57079632679489655800
#define TAU     6.28318530717958623200
#define E       2.71828182845904509080
#define LN2     0.69314718055994530942
#define LN10    2.30258509299404568402
#define SQRT2   1.41421356237309514547
#define TWO52   4503599627370496.0
public F64 Fmin(F64 a, F64 b);
public F64 Fmax(F64 a, F64 b);
public I64 Gcd(I64 a, I64 b);
public I64 Factorial(I64 n);
public F64 Fabs(F64 x);
public F64 Sqrt(F64 x);
public F64 Trunc(F64 x);
public F64 Floor(F64 x);
public F64 Ceil(F64 x);
public F64 Round(F64 x);
public F64 RoundToEven(F64 x);
public I64 LRound(F64 x);
public I64 LLRound(F64 x);
public I64 LRint(F64 x);
public F64 Fmod(F64 x, F64 y);
public F64 PowI(F64 base, I64 exp);
public F64 Exp(F64 x);
public F64 Ln(F64 x);
public F64 Log2(F64 x);
public F64 Log10(F64 x);
public F64 Exp2(F64 x);
public F64 Pow(F64 b, F64 p);
public F64 Hypot(F64 x, F64 y);
public F64 Sin(F64 x);
public F64 Cos(F64 x);
public F64 Tan(F64 x);
public F64 Atan(F64 x);
public F64 Asin(F64 x);
public F64 Acos(F64 x);
public F64 Atan2(F64 y, F64 x);
public F64 Sinh(F64 x);
public F64 Cosh(F64 x);
public F64 Tanh(F64 x);
public I64 Ilogb(F64 x);
public F64 Logb(F64 x);
public F64 Frexp(F64 f, I64 *exp);
public F64 Ldexp(F64 frac, I64 exp);
public F64 Scalbn(F64 x, I64 n);
public F64 Scalbln(F64 x, I64 n);
public F64 Mod(F64 x, F64 y);
public F64 Log(F64 x);
public F64 Modf(F64 f, F64 *ip);
public F64 Dim(F64 x, F64 y);
public F64 Remainder(F64 x, F64 y);
public F64 Remquo(F64 x, F64 y, I64 *quo);
public F64 Cbrt(F64 x);
public F64 Pow10(I64 n);
public F64 Expm1(F64 x);
public F64 Log1p(F64 x);
public F64 Asinh(F64 x);
public F64 Acosh(F64 x);
public F64 Atanh(F64 x);
public U0 Sincos(F64 x, F64 *s, F64 *c);
public F64 FMA(F64 x, F64 y, F64 z);
public F64 Nextafter(F64 x, F64 y);
public F64 Nexttoward(F64 x, F64 y);
#define SQRT_PI          1.77245385090551602730   // sqrt(pi)
#define TWO_OVER_SQRT_PI 1.12837916709551257390   // 2/sqrt(pi)
#define EULER_GAMMA      0.57721566490153286061   // Euler-Mascheroni constant
#define TWO_OVER_PI      0.63661977236758134308   // 2/pi
#define BESSEL_X0        15.0                     // series-vs-asymptotic threshold
public F64 ErfcCF(F64 ax);
public F64 Erf(F64 x);
public F64 Erfc(F64 x);
public F64 Erfinv(F64 y);
public F64 Erfcinv(F64 y);
public F64 Gamma(F64 x);
public F64 Lgamma(F64 x, I64 *sign);
public U0 BesselAsymp(F64 x, I64 nu, F64 *pj, F64 *py);
public F64 J0(F64 x);
public F64 J1(F64 x);
public F64 Y0(F64 x);
public F64 Y1(F64 x);
public F64 Jn(I64 n, F64 x);
public F64 Yn(I64 n, F64 x);

// Generic helpers (`Abs`/`Sign`/`Min`/`Max`) are templates the parser must register
// *before* any use site (generics are define-before-use), so they cannot be deferred to
// the end like an ordinary `.hc` implementation. They live in `<math.hc>`, included at
// the foot of this header — the C++ template-header idiom — so they are parsed eagerly
// with these declarations. The prototypes are listed here for the reader; the bodies are
// in the implementation file:
//
//   public T   Abs <comparable T>(T n);          // |n|, keeping T (F64 → Fabs semantics)
//   public I64 Sign<comparable T>(T n);          // -1 / 0 / 1
//   public T   Min <comparable T>(T a, T b);     // smaller, keeping T (F64 NaN handling)
//   public T   Max <comparable T>(T a, T b);     // larger, keeping T (F64 NaN handling)

#include <math.hc>

#endif
