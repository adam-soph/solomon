// Write through a pointer and verify both the original and pointer see the change.

#include <stdio.hh>
#include <unistd.hh>
I64 x = 10;
I64 *p = &x;
*p = 42;
"x=%d *p=%d\n", x, *p;
