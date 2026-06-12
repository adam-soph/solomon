// Factorial with for loop.

#include <math.hh>
#include <stdio.hh>
I64 n, fact;
for (n = 0; n <= 8; n++) {
  fact = 1;
  I64 k;
  for (k = 2; k <= n; k++)
    fact *= k;
  "%d! = %d\n", n, fact;
}
