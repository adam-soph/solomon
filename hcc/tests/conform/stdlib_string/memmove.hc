
#include <stdio.hh>
#include <string.hh>
U8 buf[16];
buf[0] = 'a'; buf[1] = 'b'; buf[2] = 'c'; buf[3] = 'd'; buf[4] = 0;
// overlapping move: shift right by 1
MemMove(buf + 1, buf, 4);
buf[0] = 'X';
buf[5] = 0;
"%s\n", buf;
