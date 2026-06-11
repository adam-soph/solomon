#include <stdlib.hc>
U8 *p = MAlloc(4);
p[0] = 'a'; p[1] = 'b'; p[2] = 'c'; p[3] = 0;
// grow the block
p = ReAlloc(p, 4, 8);
p[3] = 'd'; p[4] = 'e'; p[5] = 0;
"%s\n", p;
Free(p);
