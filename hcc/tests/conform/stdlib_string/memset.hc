
#include <stdio.hh>
#include <string.hh>
U8 buf[8];
MemSet(buf, 'x', 5);
buf[5] = 0;
"%s\n", buf;
MemSet(buf, 0, 8);
"%d\n", buf[0];
