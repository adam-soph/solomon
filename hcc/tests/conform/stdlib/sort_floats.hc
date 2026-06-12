#include <stdio.hh>
#include <stdlib.hh>
F64 *arr = MAlloc(4 * sizeof(F64));
arr[0] = 3.1; arr[1] = 1.4; arr[2] = 1.5; arr[3] = 9.2;
Sort<F64>(arr, 4, &CmpF64);
I64 i = 0;
while (i < 4) { "%f ", arr[i]; i++; }
"\n";
Free(arr);
