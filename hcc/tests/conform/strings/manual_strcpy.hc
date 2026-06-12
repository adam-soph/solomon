// manual_strcpy.hc — hand-rolled strcpy without stdlib

#include <stdio.hh>
U8 src[] = "hello";
U8 dst[16];
I64 i = 0;
while (src[i] != 0) { dst[i] = src[i]; i++; }
dst[i] = 0;
"%s\n", dst;
