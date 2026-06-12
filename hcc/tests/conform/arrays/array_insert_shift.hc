// Insert a value at index 2 by shifting elements right.

#include <stdio.hh>
I64 a[6] = {1, 2, 4, 5, 6, 0};
I64 n = 5;
I64 pos = 2, val = 3;
I64 i;
for (i = n; i > pos; i--) a[i] = a[i-1];
a[pos] = val;
n++;
for (i = 0; i < n; i++) "%d ", a[i];
"\n";
