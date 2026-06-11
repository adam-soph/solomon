#include <stdio.h>
#define N 4096
#define REPS 250
static long long a[N];
int main(void){
  long long sum = 0;
  for (long long i = 0; i < N; i++) a[i] = i*3+1;
  for (long long r = 0; r < REPS; r++)
    for (long long key = 0; key < N; key++){
      long long target = key*3+1, lo = 0, hi = N-1, found = -1;
      while (lo <= hi){
        long long mid = (lo+hi)/2;
        if (a[mid] == target){ found = mid; break; }
        else if (a[mid] < target) lo = mid+1;
        else hi = mid-1;
      }
      sum += found;
    }
  printf("%lld\n", sum);
  return 0;
}
