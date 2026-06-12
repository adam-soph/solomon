// Multiple default arguments.

#include <stdio.hh>
I64 MakeCode(I64 a, I64 b = 10, I64 c = 100)
{
  return a + b + c;
}
"%d\n", MakeCode(1);
"%d\n", MakeCode(1, 2);
"%d\n", MakeCode(1, 2, 3);
