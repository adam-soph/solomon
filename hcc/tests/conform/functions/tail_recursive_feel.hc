// Tail-recursive-feel power function.

#include <stdio.hh>
I64 PowTail(I64 base, I64 exp, I64 acc)
{
  if (exp == 0) return acc;
  return PowTail(base, exp - 1, acc * base);
}
I64 Pow(I64 b, I64 e) { return PowTail(b, e, 1); }
"%d\n", Pow(2, 10);
"%d\n", Pow(3, 4);
