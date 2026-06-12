// Heap-allocate an array, fill via pointer arithmetic, sum it.

#include <stdio.hh>
I64 n = 8;
I64 *arr = MAlloc(sizeof(I64) * n);
I64 i;
for (i = 0; i < n; i++) arr[i] = i + 1;
I64 sum = 0;
for (i = 0; i < n; i++) sum += arr[i];
"%d\n", sum;
Free(arr);
