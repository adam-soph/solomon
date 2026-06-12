#include <stdio.hh>
#include <stdlib.hh>
I64 *arr = MAlloc(5 * sizeof(I64));
arr[0] = 5; arr[1] = 2; arr[2] = 8; arr[3] = 1; arr[4] = 4;
Sort<I64>(arr, 5, &CmpI64);
I64 i = 0;
while (i < 5) { "%d ", arr[i]; i++; }
"\n";
Free(arr);
