
#include <stdio.hh>
#include <string.hh>
U8 buf[32];
StrCpy(buf, "hello");
StrNCat(buf, " worldXXX", 6);
"%s\n", buf;
