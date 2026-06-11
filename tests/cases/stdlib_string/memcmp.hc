#include <string.hc>
U8 a[4]; U8 b[4];
a[0] = 1; a[1] = 2; a[2] = 3; a[3] = 4;
b[0] = 1; b[1] = 2; b[2] = 3; b[3] = 4;
"%d\n", MemCmp(a, b, 4);
b[2] = 99;
"%d\n", MemCmp(a, b, 4);
"%d\n", MemCmp(b, a, 4);
