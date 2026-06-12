
#include <math.hh>
#include <stdio.hh>
#include <stdlib.hh>
U0 Main() {
  "%x %g\n", Float64bits(1.0), Float64frombits(4611686018427387904);
  "%d %d %d %d\n", IsNaN(NaN()), IsInf(Inf(1), 1), IsInf(Inf(-1), -1), Signbit(-0.0);
  "%.1f %.1f\n", Copysign(3.0, -1.0), Copysign(-3.0, 1.0);
  "%d %.1f\n", Ilogb(8.0), Logb(8.0);
  I64 e; F64 fr = Frexp(12.0, &e); "%.4f %d %.1f\n", fr, e, Ldexp(0.75, 4);
  F64 ip; F64 fp = Modf(3.75, &ip); "%.2f %.2f %.1f\n", ip, fp, Dim(5.0, 2.0);
  "%.6f %.6f %.6f\n", Cbrt(27.0), Expm1(1e-6), Log1p(1e-6);
  "%.6f %.6f %.6f\n", Asinh(1.0), Acosh(2.0), Atanh(0.5);
  "%.6f %.6f %.1f\n", Remainder(5.3, 2.0), FMA(2.0, 3.0, 4.0), Pow10(3);
  F64 s, c; Sincos(0.5, &s, &c); "%.6f %.6f\n", s, c;
}
Main;
