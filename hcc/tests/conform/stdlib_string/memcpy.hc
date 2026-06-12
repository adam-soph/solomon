
#include <stdio.hh>
#include <string.hh>
U8 src[8];
U8 dst[8];
src[0] = 'a'; src[1] = 'b'; src[2] = 'c'; src[3] = 0;
MemCpy(dst, src, 4);
"%s\n", dst;
