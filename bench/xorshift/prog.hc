// xorshift64 PRNG — accumulate the low bits of a long pseudo-random stream (shift/xor heavy).
#include <stdio.hc>

#define REPS 5000000
U64 x = 0x9E3779B97F4A7C15;
I64 r, acc = 0;
for (r = 0; r < REPS; r++) {
  x = x ^ (x << 13);
  x = x ^ (x >> 7);
  x = x ^ (x << 17);
  acc += x & 0xFFFF;
}
"%d\n", acc;
