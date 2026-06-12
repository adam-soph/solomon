// Power via repeated multiplication.

#include <stdio.hh>
I64 Pow(I64 base, I64 exp)
{
  I64 result = 1, i;
  for (i = 0; i < exp; i++)
    result *= base;
  return result;
}
"%d\n", Pow(2, 10);
"%d\n", Pow(3, 5);
"%d\n", Pow(7, 3);
