// Two arrays indexed by the same induction variable — two strength-reduction sites in one loop.

#include <stdio.hh>
#define N 100
I64 x[N], y[N];
I64 i, dot = 0;
for (i = 0; i < N; i++) { x[i] = i; y[i] = N - i; }
for (i = 0; i < N; i++) dot += x[i] * y[i];
"%d\n", dot;
