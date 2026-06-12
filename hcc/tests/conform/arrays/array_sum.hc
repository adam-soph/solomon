// Sum all elements of an array.

#include <stdio.hh>
I64 a[6] = {1, 2, 3, 4, 5, 6};
I64 sum = 0, i;
for (i = 0; i < 6; i++) sum += a[i];
"%d\n", sum;
