// do-while computing a sum.

#include <stdio.hh>
I64 sum = 0, k = 1;
do {
  sum += k;
  k++;
} while (k <= 10);
"sum=%d\n", sum;
