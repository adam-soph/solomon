#include <stdio.h>
#define N 300000
static unsigned char sieve[N];
int main(void){
  long long count = 0;
  for (long long i = 2; i < N; i++)
    if (!sieve[i]) { count++; for (long long j = i+i; j < N; j += i) sieve[j] = 1; }
  printf("%lld\n", count);
  return 0;
}
