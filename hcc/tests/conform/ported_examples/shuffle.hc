// shuffle.hc — a Fisher-Yates shuffle of 0..N-1 driven by the deterministic
// RandU64 PRNG. Because RandU64 is seeded the same in both backends, the result
// is reproducible and identical between the interpreter and native code.

#include <stdio.hh>
#include <stdlib.hh>
#include <stdlib.hh>   // RandU64

#define N 10

U0 Main() {
  I64 a[N];
  I64 i;
  for (i = 0; i < N; i++)
    a[i] = i;

  // For i from N-1 down to 1, swap a[i] with a random earlier element. The mask
  // clears the sign bit so the modulo index is non-negative.
  for (i = N - 1; i > 0; i--) {
    I64 j = (RandU64() & 0x7FFFFFFFFFFFFFFF) % (i + 1);
    I64 tmp = a[i];
    a[i] = a[j];
    a[j] = tmp;
  }

  for (i = 0; i < N; i++)
    "%d ", a[i];
  "\n";

  // The shuffle is a permutation, so the sum is invariant (0+...+9 == 45).
  I64 sum = 0;
  for (i = 0; i < N; i++)
    sum += a[i];
  "sum=%d\n", sum;
}

Main;
