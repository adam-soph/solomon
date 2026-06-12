// Default argument value.

#include <stdio.hh>
I64 Inc(I64 x, I64 step = 1)
{
  return x + step;
}
"%d\n", Inc(5);
"%d\n", Inc(5, 3);
"%d\n", Inc(10);
