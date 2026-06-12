// Initializer list with expressions (not just literals).

#include <stdio.hh>
I64 n = 3;
I64 a[3] = {n, n*2, n*3};
"%d %d %d\n", a[0], a[1], a[2];
