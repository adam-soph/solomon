#include <string.hc>
U8 buf[8];
buf[0] = 'a'; buf[1] = 'b'; buf[2] = 'c'; buf[3] = 'd'; buf[4] = 0;
U8 *p = MemFind(buf, 'c', 4);
if (p) "%c\n", *p;
else "not found\n";
p = MemFind(buf, 'z', 4);
if (p) "%c\n", *p;
else "not found\n";
