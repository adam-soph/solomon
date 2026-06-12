#include <stdio.hh>
#include <stdlib.hh>
I64 *arr = MAlloc(3 * sizeof(I64));
arr[0] = 2; arr[1] = 4; arr[2] = 6;
I64 key = 3;
I64 *p = BSearch<I64>(&key, arr, 3, &CmpI64);
if (p) "%d\n", *p;
else "not found\n";
Free(arr);
