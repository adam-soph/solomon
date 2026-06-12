#include <stdio.h>
#define N 20000
static long long data[N];
static void quick(long long *a, long long lo, long long hi) {
  if (lo >= hi) return;
  long long p = a[(lo+hi)/2], i = lo, j = hi;
  while (i <= j) {
    while (a[i] < p) i++;
    while (a[j] > p) j--;
    if (i <= j) { long long t = a[i]; a[i] = a[j]; a[j] = t; i++; j--; }
  }
  quick(a, lo, j);
  quick(a, i, hi);
}
int main(void){
  long long total = 0;
  for (int rep = 0; rep < 8; rep++) {
    for (long long i = 0; i < N; i++) data[i] = (i*1103 + 12345) % 100003;
    quick(data, 0, N-1);
    for (long long i = 0; i < N; i++) total += (i % 1000) * data[i];
  }
  printf("%lld\n", total);
  return 0;
}
