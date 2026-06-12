// Kernighan population-count idiom recognition. The first loop is the exact `while (x) { x = x
// & (x - 1); c++; }` shape the backend rewrites to a constant-time bit-twiddling popcount; the
// remaining cases are near-misses that must keep computing the same values the normal way (so
// native == interp == golden pins both the rewrite and the matcher's strictness).

// Exact idiom: count set bits of each value.

#include <stdio.hh>
I64 PopCount(U64 x) {
  I64 c = 0;
  while (x) { x = x & (x - 1); c++; }
  return c;
}

// Near-miss: counts *clears* of two bits at a time via x & (x-1) but increments by 2 — must NOT
// fold to a popcount (it would give a different answer for odd popcounts).
I64 CountBy2(U64 x) {
  I64 c = 0;
  while (x) { x = x & (x - 1); c += 2; }
  return c;
}

// Near-miss: the running value escapes the loop (returned), so the loop can't be deleted.
U64 ClearAllBits(U64 x) {
  while (x) { x = x & (x - 1); }
  return x;
}

"%d\n", PopCount(0);
"%d\n", PopCount(1);
"%d\n", PopCount(0xFFFFFFFFFFFFFFFF);   // all 64 bits
"%d\n", PopCount(0x8000000000000000);   // top bit only
"%d\n", PopCount(0xDEADBEEFCAFEBABE);
"%d\n", PopCount(0x5555555555555555);   // 32 bits
"%d\n", CountBy2(0xDEADBEEFCAFEBABE);   // near-miss path
"%d\n", ClearAllBits(0xFF);             // near-miss path → 0
