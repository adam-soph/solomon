
#include <math.hh>
#include <stdio.hh>
#include <stdlib.hh>
U0 Main() {
  "%d %d\n", Min(3, 9), Max(3, 9);          // integers stay I64
  "%.2f %.2f\n", Min(2.5, 1.5), Max(2.5, 1.5); // floats are F64, not truncated
  F64 nan = NaN();                              // fmin/fmax: NaN -> the other
  "%.1f %.1f %.1f\n", Max(5.0, nan), Max(nan, 5.0), Min(nan, 5.0);
}
Main;
