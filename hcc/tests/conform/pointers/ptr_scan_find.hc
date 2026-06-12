// Scan an array with a pointer to find the first element > threshold.

#include <stdio.hh>
I64 arr[6] = {2, 5, 1, 8, 3, 7};
I64 *p = arr;
I64 *stop = arr + 6;
I64 thresh = 6;
while (p < stop && *p <= thresh) p++;
"%d\n", *p;
"%d\n", p - arr;
