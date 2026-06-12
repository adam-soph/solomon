// Binary search — sum the found index of every key over many repetitions (branch-heavy).
#include <stdio.hc>

#define N 4096
#define REPS 250
I64 a[N];
I64 i, key, r, sum = 0;
for (i = 0; i < N; i++) a[i] = i * 3 + 1;          // sorted ascending
for (r = 0; r < REPS; r++)
  for (key = 0; key < N; key++) {
    I64 target = key * 3 + 1, lo = 0, hi = N - 1, found = -1;
    while (lo <= hi) {
      I64 mid = (lo + hi) / 2;
      if (a[mid] == target) { found = mid; break; }
      else if (a[mid] < target) lo = mid + 1;
      else hi = mid - 1;
    }
    sum += found;
  }
"%d\n", sum;
