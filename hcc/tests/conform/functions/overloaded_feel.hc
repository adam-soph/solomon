// Two functions with different param counts (no overloading, just two funcs).

#include <stdio.hh>
I64 Max2(I64 a, I64 b)
{
  return (a > b) ? a : b;
}
I64 Max3(I64 a, I64 b, I64 c)
{
  return Max2(Max2(a, b), c);
}
"%d\n", Max2(3, 7);
"%d\n", Max3(5, 1, 9);
"%d\n", Max3(3, 3, 3);
