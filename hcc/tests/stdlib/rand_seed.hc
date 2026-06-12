#include <stdio.hh>
#include <stdlib.hh>
U0 Main() {
  SeedRand(1); I64 a = RandU64();
  SeedRand(1); I64 b = RandU64();
  SeedRand(2); I64 c = RandU64();
  "%d %d\n", a == b, a != c;
}
Main;
