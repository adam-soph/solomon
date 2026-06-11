// Longest-common-subsequence length via 2D dynamic programming (nested loops, 2D arrays).
#define N 150
#define REPS 550
U8 a[N], b[N];
I64 dp[N + 1][N + 1];
I64 i, j, r, total = 0;
for (i = 0; i < N; i++) { a[i] = (i * 7 + 3) % 5; b[i] = (i * 3 + 1) % 5; }
for (r = 0; r < REPS; r++) {
  for (i = 0; i <= N; i++) dp[i][0] = 0;
  for (j = 0; j <= N; j++) dp[0][j] = 0;
  for (i = 1; i <= N; i++)
    for (j = 1; j <= N; j++) {
      if (a[i - 1] == b[j - 1]) dp[i][j] = dp[i - 1][j - 1] + 1;
      else if (dp[i - 1][j] > dp[i][j - 1]) dp[i][j] = dp[i - 1][j];
      else dp[i][j] = dp[i][j - 1];
    }
  total += dp[N][N];
}
"%d\n", total;
