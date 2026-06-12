// Function both reads and modifies through a pointer.

#include <stdio.hh>
#include <stdlib.hh>
U0 DoubleIt(I64 *p) { *p = *p * 2; }

I64 x = 7;
DoubleIt(&x);
"%d\n", x;
DoubleIt(&x);
"%d\n", x;
