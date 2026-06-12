// Diagonal access `a[i][i]` — both indices are the same induction variable, the chained-base
// case strength reduction must handle (the inner reduces; the outer reuses it).

#include <stdio.hh>
#define N 12
I64 a[N][N];
I64 i, j, trace = 0;
for (i = 0; i < N; i++) for (j = 0; j < N; j++) a[i][j] = i * N + j;
for (i = 0; i < N; i++) trace += a[i][i];
"%d\n", trace;
