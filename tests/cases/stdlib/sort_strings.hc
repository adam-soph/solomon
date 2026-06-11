#include <stdlib.hc>
#include <string.hc>
U8 **arr = MAlloc(4 * sizeof(U8 *));
arr[0] = "banana"; arr[1] = "apple"; arr[2] = "cherry"; arr[3] = "date";
Sort<U8 *>(arr, 4, &CmpStr);
I64 i = 0;
while (i < 4) { "%s\n", arr[i]; i++; }
Free(arr);
