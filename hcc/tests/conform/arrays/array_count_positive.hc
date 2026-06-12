// Count elements satisfying a predicate (positive values).

#include <stdio.hh>
I64 a[7] = {-3, 2, -1, 0, 5, -2, 4};
I64 cnt = 0, i;
for (i = 0; i < 7; i++)
  if (a[i] > 0) cnt++;
"%d\n", cnt;
