
#include <stdio.hh>
#include <string.hh>
U8 buf[32];
StrCpy(buf, "hello");
StrCat(buf, " world");
"%s\n", buf;
"%d\n", StrLen(buf);
