#include <stdlib.hc>
U8 *p = CAlloc(8);
I64 ok = 1, i = 0;
while (i < 8) { if (p[i] != 0) ok = 0; i++; }
"%d\n", ok;
Free(p);
