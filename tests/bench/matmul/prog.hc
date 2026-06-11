// Integer matrix multiply, repeated; print the accumulated diagonal trace.
#define N 80
I64 a[N][N], b[N][N], c[N][N];
I64 i, j, k, rep, total = 0;
for (i = 0; i < N; i++) for (j = 0; j < N; j++) { a[i][j] = (i * 7 + j) % 13; b[i][j] = (i + j * 5) % 11; }
for (rep = 0; rep < 25; rep++) {
  for (i = 0; i < N; i++) for (j = 0; j < N; j++) { I64 s = 0; for (k = 0; k < N; k++) s += a[i][k] * b[k][j]; c[i][j] = s; }
  for (i = 0; i < N; i++) total += c[i][i];
}
"%d\n", total;
