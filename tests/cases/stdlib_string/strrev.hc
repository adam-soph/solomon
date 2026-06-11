#include <string.hc>
U8 buf[32];
StrCpy(buf, "abcde");
StrRev(buf);
"%s\n", buf;
StrCpy(buf, "a");
StrRev(buf);
"%s\n", buf;
