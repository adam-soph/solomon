#include <stdlib.hc>
I64 *arr = MAlloc(4 * sizeof(I64));
arr[0] = 1; arr[1] = 2; arr[2] = 3; arr[3] = 4;
Sort<I64>(arr, 4, &CmpI64);
I64 i = 0;
while (i < 4) { "%d ", arr[i]; i++; }
"\n";
Free(arr);
