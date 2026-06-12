// matrix.hc — fixed-size 3x3 matrix math with nested arrays, nested loops,
// arrays passed (by reference) to functions, and F64 arithmetic.


#include <stdio.hh>
#include <stdlib.hh>
#define N 3

U0 MatMul(F64 a[N][N], F64 b[N][N], F64 out[N][N]) {
  I64 i, j, k;
  for (i = 0; i < N; i++) {
    for (j = 0; j < N; j++) {
      F64 sum = 0.0;
      for (k = 0; k < N; k++)
        sum += a[i][k] * b[k][j];
      out[i][j] = sum;
    }
  }
}

F64 Trace(F64 m[N][N]) {
  F64 t = 0.0;
  I64 i;
  for (i = 0; i < N; i++)
    t += m[i][i];
  return t;
}

U0 Main() {
  F64 a[N][N];
  F64 b[N][N];
  F64 c[N][N];

  I64 i, j;
  // a = 2 * identity, b = all ones.
  for (i = 0; i < N; i++) {
    for (j = 0; j < N; j++) {
      a[i][j] = (i == j) ? 2.0 : 0.0;
      b[i][j] = 1.0;
    }
  }

  MatMul(a, b, c);
  // c is then all 2.0, so its trace is 6.
  "trace = %f\n", Trace(c);
  "c[0][0]=%f c[2][1]=%f\n", c[0][0], c[2][1];
}

Main;
