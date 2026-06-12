// macro_array_size.hc — macro used as array size

#include <stdio.hh>
#define CAPACITY 8
I64 buf[CAPACITY];
I64 i;
for (i = 0; i < CAPACITY; i++) buf[i] = i * 3;
"%d %d %d\n", buf[0], buf[4], buf[7];
