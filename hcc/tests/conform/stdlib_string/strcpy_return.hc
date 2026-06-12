
#include <stdio.hh>
#include <string.hh>
U8 buf[16];
U8 *r = StrCpy(buf, "test");
"%s\n", r;
// return value == buf
"%d\n", r == buf;
