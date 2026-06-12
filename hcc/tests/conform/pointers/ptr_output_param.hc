// Function writes results through two output-pointer parameters.

#include <stdio.hh>
#include <stdlib.hh>
U0 MinMax(I64 *arr, I64 n, I64 *out_min, I64 *out_max) {
  *out_min = arr[0];
  *out_max = arr[0];
  I64 i;
  for (i = 1; i < n; i++) {
    if (arr[i] < *out_min) *out_min = arr[i];
    if (arr[i] > *out_max) *out_max = arr[i];
  }
}

I64 data[6] = {3, 1, 4, 1, 5, 9};
I64 lo, hi;
MinMax(data, 6, &lo, &hi);
"min=%d max=%d\n", lo, hi;
