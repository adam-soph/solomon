// NULL pointer check via explicit comparison.

#include <stdio.hh>
I64 *p = NULL;
I64 x = 5;
I64 *q = &x;
"%d %d\n", p == NULL, q != NULL;
