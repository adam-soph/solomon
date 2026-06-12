// Function taking a pointer and mutating through it.

#include <stdio.hh>
#include <stdlib.hh>
U0 Double(I64 *p) { *p = *p * 2; }
U0 Increment(I64 *p) { (*p)++; }

I64 x = 5;
Double(&x);
"%d\n", x;
Increment(&x);
"%d\n", x;
