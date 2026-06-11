// Population count (Kernighan's `x & (x-1)`) summed over a range, repeated.
#define N 1000000
#define REPS 5
I64 i, r, total = 0;
for (r = 0; r < REPS; r++)
  for (i = 0; i < N; i++) {
    I64 x = i, c = 0;
    while (x) { x = x & (x - 1); c++; }
    total += c;
  }
"%d\n", total;
