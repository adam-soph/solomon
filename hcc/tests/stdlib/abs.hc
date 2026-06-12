
#include <math.hh>
#include <stdio.hh>
#include <stdlib.hh>
U0 Main() {
  "%d %d\n", Abs(-7), Abs(7);              // integers stay I64
  "%.2f %.2f\n", Abs(-3.5), Abs(3.5);       // floats are F64, not truncated
  "%.1f\n", Abs(-0.0);                       // IEEE: +0.0, not -0.0
  "%d\n", IsNaN(Abs(NaN()));                 // NaN stays NaN
}
Main;
