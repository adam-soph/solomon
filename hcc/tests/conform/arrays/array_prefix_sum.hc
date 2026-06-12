// Prefix (inclusive) sum array.

#include <stdio.hh>
I64 a[5] = {1, 2, 3, 4, 5};
I64 pre[5];
pre[0] = a[0];
I64 i;
for (i = 1; i < 5; i++) pre[i] = pre[i-1] + a[i];
for (i = 0; i < 5; i++) "%d ", pre[i];
"\n";
