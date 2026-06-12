
#include <stdio.hh>
#include <string.hh>
U8 buf[8];
MemSet(buf, 'A', 8);
buf[4] = 0;
"%s\n", buf;
