
#include <stdio.hh>
#include <string.hh>
U8 buf[12];
buf[0] = 'h'; buf[1] = 'e'; buf[2] = 'l'; buf[3] = 'l'; buf[4] = 'o'; buf[5] = 0;
// non-overlapping copy to offset 6
MemCpy(buf + 6, buf, 6);
"%s\n", buf + 6;
