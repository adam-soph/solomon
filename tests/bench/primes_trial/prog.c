#include <stdio.h>
#define N 200000
int main(void){
  long long count = 0;
  for (long long n = 2; n < N; n++) {
    int isp = 1;
    for (long long d = 2; d*d <= n; d++) if (n % d == 0) { isp = 0; break; }
    if (isp) count++;
  }
  printf("%lld\n", count);
  return 0;
}
