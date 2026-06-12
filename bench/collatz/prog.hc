// Longest Collatz chain for starting values below N.
#include <stdio.hc>

#define N 100000
I64 n, best = 0;
for (n = 1; n < N; n++) {
  I64 x = n, steps = 0;
  while (x != 1) { if (x & 1) x = 3 * x + 1; else x = x / 2; steps++; }
  if (steps > best) best = steps;
}
"%d\n", best;
