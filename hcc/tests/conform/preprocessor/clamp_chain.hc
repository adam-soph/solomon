// clamp_chain.hc — CLAMP using separate MAX and MIN macros

#include <stdio.hh>
#define MAX2(a, b) ((a) >= (b) ? (a) : (b))
#define MIN2(a, b) ((a) <= (b) ? (a) : (b))
#define CLAMP(x, lo, hi) MAX2((lo), MIN2((x), (hi)))
I64 i;
for (i = -2; i <= 12; i += 2)
  "%d ", CLAMP(i, 0, 10);
"\n";
