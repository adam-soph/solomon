// 2x2 matrix multiply.

#include <stdio.hh>
#define N 2
I64 a[N][N] = {{1,2},{3,4}};
I64 b[N][N] = {{5,6},{7,8}};
I64 c[N][N];
I64 i, j, k;
for (i = 0; i < N; i++)
  for (j = 0; j < N; j++) {
    c[i][j] = 0;
    for (k = 0; k < N; k++) c[i][j] += a[i][k] * b[k][j];
  }
"%d %d %d %d\n", c[0][0], c[0][1], c[1][0], c[1][1];
