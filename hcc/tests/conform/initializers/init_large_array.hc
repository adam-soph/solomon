// Larger brace initializer (10 elements) and sum.

#include <stdio.hh>
I64 a[10] = {1,2,3,4,5,6,7,8,9,10};
I64 s = 0, i;
for (i = 0; i < 10; i++) s += a[i];
"%d\n", s;
