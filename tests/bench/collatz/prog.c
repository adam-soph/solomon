#include <stdio.h>
#define N 100000
int main(void){
  long long best = 0;
  for (long long n = 1; n < N; n++) {
    long long x = n, steps = 0;
    while (x != 1) { if (x & 1) x = 3*x + 1; else x = x/2; steps++; }
    if (steps > best) best = steps;
  }
  printf("%lld\n", best);
  return 0;
}
