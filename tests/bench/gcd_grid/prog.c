#include <stdio.h>
static long long gcd(long long a, long long b){ while (b) { long long t = b; b = a % b; a = t; } return a; }
int main(void){
  long long total = 0;
  for (long long i = 1; i <= 900; i++) for (long long j = 1; j <= 900; j++) total += gcd(i, j);
  printf("%lld\n", total);
  return 0;
}
