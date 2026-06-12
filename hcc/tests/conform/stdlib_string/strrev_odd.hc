
#include <stdio.hh>
#include <string.hh>
U8 buf[8];
StrCpy(buf, "abc");
StrRev(buf);
"%s\n", buf;
StrCpy(buf, "abcd");
StrRev(buf);
"%s\n", buf;
