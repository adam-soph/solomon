// Dot product of two F64 arrays.

#include <stdio.hh>
F64 a[4] = {1.0, 2.0, 3.0, 4.0};
F64 b[4] = {4.0, 3.0, 2.0, 1.0};
F64 dot = 0.0;
I64 i;
for (i = 0; i < 4; i++) dot += a[i] * b[i];
"%f\n", dot;
