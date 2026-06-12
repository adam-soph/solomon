
#include <math.hh>
#include <stdio.hh>
#include <stdlib.hh>
U0 Main() {
  "%.1f %.1f %.1f %.1f\n", Round(2.5), Round(-2.5), Round(0.5), Round(-3.5);
  "%.1f %.1f %.1f %.1f\n", Floor(2.7), Floor(-2.3), Ceil(2.1), Ceil(-2.9);
  "%.1f\n", PowI(2.0, 10);
  "%d %d %d %d\n", Gcd(48, 36), Factorial(6), Min(3, 9), Max(3, 9);
}
Main;
