#include <stdio.hh>
#include <stdlib.hh>
I64 *arr = MAlloc(5 * sizeof(I64));
arr[0] = 1; arr[1] = 3; arr[2] = 5; arr[3] = 7; arr[4] = 9;
I64 key = 5;
I64 *p = BSearch<I64>(&key, arr, 5, &CmpI64);
if (p) "%d\n", *p;
else "not found\n";
key = 4;
p = BSearch<I64>(&key, arr, 5, &CmpI64);
if (p) "%d\n", *p;
else "not found\n";
Free(arr);
