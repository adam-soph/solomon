// Pointer-to-pointer: reading and writing through two levels of indirection.

#include <stdio.hh>
I64 x = 99;
I64 *p = &x;
I64 **pp = &p;
"%d\n", **pp;
**pp = 77;
"%d %d\n", x, *p;
