// Recursive GCD.

#include <stdio.hh>
I64 GCD(I64 a, I64 b)
{
  if (b == 0) return a;
  return GCD(b, a % b);
}
"%d\n", GCD(48, 36);
"%d\n", GCD(100, 75);
"%d\n", GCD(17, 13);
