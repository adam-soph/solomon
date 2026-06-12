// Two pointers alias the same variable; a write through one is visible through the other.

#include <stdio.hh>
I64 x = 5;
I64 *p = &x;
I64 *q = &x;
*p = 10;
"%d %d\n", *p, *q;
*q = 20;
"%d\n", x;
