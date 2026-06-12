// U0 void functions that print.

#include <stdio.hh>
#include <stdlib.hh>
U0 PrintBanner(I64 n)
{
  "=== %d ===\n", n;
}

U0 PrintRange(I64 lo, I64 hi)
{
  I64 i;
  for (i = lo; i <= hi; i++)
    "%d ", i;
  "\n";
}

PrintBanner(1);
PrintRange(1, 5);
PrintBanner(2);
PrintRange(10, 15);
