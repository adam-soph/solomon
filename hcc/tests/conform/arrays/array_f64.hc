// Array of F64: fill, sum, average.

#include <stdio.hh>
F64 a[4] = {1.5, 2.5, 3.5, 4.5};
F64 sum = 0.0;
I64 i;
for (i = 0; i < 4; i++) sum += a[i];
"%f\n", sum;
"%f\n", sum / 4.0;
