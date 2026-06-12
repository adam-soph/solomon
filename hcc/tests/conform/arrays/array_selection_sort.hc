// Selection sort; print before and after.

#include <stdio.hh>
I64 a[5] = {4, 2, 7, 1, 5};
I64 i, j, min_idx;
for (i = 0; i < 5; i++) "%d ", a[i];
"\n";
for (i = 0; i < 4; i++) {
  min_idx = i;
  for (j = i+1; j < 5; j++)
    if (a[j] < a[min_idx]) min_idx = j;
  I64 t = a[i]; a[i] = a[min_idx]; a[min_idx] = t;
}
for (i = 0; i < 5; i++) "%d ", a[i];
"\n";
