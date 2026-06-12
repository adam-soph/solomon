#include <stdio.hh>
#include <stdlib.hh>
SeedRand(123);
U64 a = RandU64();
U64 b = RandU64();
// Reseed and verify same sequence
SeedRand(123);
"%d\n", RandU64() == a;
"%d\n", RandU64() == b;
