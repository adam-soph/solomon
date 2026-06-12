// Find both min and max of an array with a single pass.

#include <stdio.hh>
I64 a[8] = {4, 9, 2, 6, 1, 8, 3, 7};
I64 lo = a[0], hi = a[0], i;
for (i = 1; i < 8; i++) {
  if (a[i] < lo) lo = a[i];
  if (a[i] > hi) hi = a[i];
}
"min=%d max=%d\n", lo, hi;
