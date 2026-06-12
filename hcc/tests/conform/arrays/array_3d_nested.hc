// Triple-nested 3D array — three induction variables and nested address strength reduction.

#include <stdio.hh>
#define N 8
I64 t[N][N][N];
I64 i, j, k, sum = 0;
for (i = 0; i < N; i++) for (j = 0; j < N; j++) for (k = 0; k < N; k++)
  t[i][j][k] = i + j + k;
for (i = 0; i < N; i++) for (j = 0; j < N; j++) for (k = 0; k < N; k++)
  sum += t[i][j][k];
"%d\n", sum;
