#include <stdio.h>
#define N 80
static long long a[N][N], b[N][N], c[N][N];
int main(void){
  long long total = 0;
  for (long long i = 0; i < N; i++) for (long long j = 0; j < N; j++) { a[i][j] = (i*7+j)%13; b[i][j] = (i+j*5)%11; }
  for (int rep = 0; rep < 25; rep++) {
    for (long long i = 0; i < N; i++) for (long long j = 0; j < N; j++) { long long s = 0; for (long long k = 0; k < N; k++) s += a[i][k]*b[k][j]; c[i][j] = s; }
    for (long long i = 0; i < N; i++) total += c[i][i];
  }
  printf("%lld\n", total);
  return 0;
}
