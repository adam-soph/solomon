#include <stdlib.hc>
I64 *arr = MAlloc(1 * sizeof(I64));
arr[0] = 42;
Sort<I64>(arr, 1, &CmpI64);
"%d\n", arr[0];
Free(arr);
