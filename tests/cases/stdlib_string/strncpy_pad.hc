#include <string.hc>
U8 buf[8];
MemSet(buf, 'X', 8);
// StrNCpy pads with NULs to n when src is shorter
StrNCpy(buf, "hi", 6);
// check that buf[2..5] are NUL
I64 ok = 1, i = 2;
while (i < 6) { if (buf[i] != 0) ok = 0; i++; }
"%d\n", ok;
