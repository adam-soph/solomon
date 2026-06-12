// Count primes below N by trial division (d*d <= n).
#include <stdio.hc>

#define N 200000
I64 n, count = 0;
for (n = 2; n < N; n++) {
  I64 isp = 1, d;
  for (d = 2; d * d <= n; d++) if (n % d == 0) { isp = 0; break; }
  if (isp) count++;
}
"%d\n", count;
