
#include <math.hh>
#include <stdio.hh>
#include <stdlib.hh>
U0 Main() {
  "%.10f %.10f %.10f\n", Erf(0.5), Erf(1.0), Erf(2.0);
  "%.10g %.10g\n", Erfc(2.0), Erfc(3.0);
  "%.10f %.10f %.10f\n", Erfinv(0.5), Erfinv(0.9), Erfcinv(0.5);
  "%.10f %.10f %.10f\n", Gamma(0.5), Gamma(5.0), Gamma(-0.5);
  I64 s1, s2, s3;
  F64 l1 = Lgamma(5.0, &s1), l2 = Lgamma(0.5, &s2), l3 = Lgamma(-0.5, &s3);
  "%.10f %d %.10f %d %.10f %d\n", l1, s1, l2, s2, l3, s3;
}
Main;
