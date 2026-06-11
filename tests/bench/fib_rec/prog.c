#include <stdio.h>
long long Fib(long long n){ if (n < 2) return n; return Fib(n-1)+Fib(n-2); }
int main(void){
  long long sum = 0;
  for (long long r = 0; r < 3; r++) sum += Fib(32);
  printf("%lld\n", sum);
  return 0;
}
