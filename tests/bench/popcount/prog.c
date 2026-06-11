#include <stdio.h>
#define N 1000000
#define REPS 5
int main(void){
  long long total = 0;
  for (long long r = 0; r < REPS; r++)
    for (long long i = 0; i < N; i++){
      long long x = i, c = 0;
      while (x){ x = x & (x-1); c++; }
      total += c;
    }
  printf("%lld\n", total);
  return 0;
}
