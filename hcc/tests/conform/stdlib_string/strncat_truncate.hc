
#include <stdio.hh>
#include <string.hh>
U8 buf[16];
StrCpy(buf, "ab");
StrNCat(buf, "cdefgh", 3);
"%s\n", buf;
"%d\n", StrLen(buf);
