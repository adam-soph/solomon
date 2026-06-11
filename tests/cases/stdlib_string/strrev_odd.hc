#include <string.hc>
U8 buf[8];
StrCpy(buf, "abc");
StrRev(buf);
"%s\n", buf;
StrCpy(buf, "abcd");
StrRev(buf);
"%s\n", buf;
