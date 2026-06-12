// U0 function with implicit return (no explicit return statement).

#include <stdio.hh>
#include <stdlib.hh>
U0 PrintSquares(I64 n)
{
  I64 i;
  for (i = 1; i <= n; i++)
    "%d ", i * i;
  "\n";
}
PrintSquares(5);
PrintSquares(3);
