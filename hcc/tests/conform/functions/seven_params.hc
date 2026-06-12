// 7-parameter function (one past the 6-register boundary on most ABIs).

#include <stdio.hh>
I64 Sum7(I64 a, I64 b, I64 c, I64 d, I64 e, I64 f, I64 g)
{
  return a + b + c + d + e + f + g;
}
"%d\n", Sum7(1, 2, 3, 4, 5, 6, 7);
