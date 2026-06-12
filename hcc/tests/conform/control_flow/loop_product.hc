// Loop computing factorial via product.

#include <stdio.hh>
I64 fact = 1, i;
for (i = 1; i <= 10; i++)
  fact *= i;
"%d\n", fact;
