// Array of pointers: each element points to a different scalar.

#include <stdio.hh>
I64 a = 1, b = 2, c = 3;
I64 *ptrs[3];
ptrs[0] = &a;
ptrs[1] = &b;
ptrs[2] = &c;
"%d %d %d\n", *ptrs[0], *ptrs[1], *ptrs[2];
*ptrs[1] = 99;
"%d\n", b;
