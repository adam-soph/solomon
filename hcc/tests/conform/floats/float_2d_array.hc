// A float 2D array reduced with nested loops — float loads/stores over integer-indexed
// addresses (the address optimizations apply to the indexing, the data stays F64).

#include <stdio.hh>
#define N 6
F64 m[N][N];
I64 i, j;
F64 total = 0.0;
for (i = 0; i < N; i++) for (j = 0; j < N; j++) m[i][j] = (i + 1) * 0.5 + (j + 1) * 0.25;
for (i = 0; i < N; i++) for (j = 0; j < N; j++) total += m[i][j];
"%.4f\n", total;
