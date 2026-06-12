// Apply a function-pointer n times.

#include <stdio.hh>
I64 ApplyN(I64 (*f)(I64), I64 x, I64 n)
{
  I64 i;
  for (i = 0; i < n; i++)
    x = f(x);
  return x;
}
I64 Inc(I64 x) { return x + 1; }
I64 Dbl(I64 x) { return x * 2; }
"%d\n", ApplyN(&Inc, 0, 10);
"%d\n", ApplyN(&Dbl, 1, 8);
