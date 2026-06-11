#include <stdio.h>
#define REPS 5000000
int main(void){
  unsigned long long x = 0x9E3779B97F4A7C15ULL;
  long long acc = 0;
  for (long long r = 0; r < REPS; r++){
    x ^= x << 13; x ^= x >> 7; x ^= x << 17;
    acc += x & 0xFFFF;
  }
  printf("%lld\n", acc);
  return 0;
}
