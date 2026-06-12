// large_array_field.hc — class with a larger array field

#include <stdio.hh>
class Buf { I64 data[8]; };
Buf b;
I64 i;
for (i = 0; i < 8; i++) b.data[i] = i + 1;
I64 sum = 0;
for (i = 0; i < 8; i++) sum = sum + b.data[i];
"%d\n", sum;
