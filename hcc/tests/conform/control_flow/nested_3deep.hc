// 3-deep nested loops.

#include <stdio.hh>
I64 i, j, k, count = 0;
for (i = 0; i < 3; i++) {
  for (j = 0; j < 3; j++) {
    for (k = 0; k < 3; k++) {
      count++;
    }
  }
}
"count=%d\n", count;
