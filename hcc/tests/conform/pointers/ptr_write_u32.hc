// Write through a U32* pointer.

#include <stdio.hh>
#include <unistd.hh>
U32 x = 0;
U32 *p = &x;
*p = 0xDEADBEEF;
"%u\n", (U64)*p;
