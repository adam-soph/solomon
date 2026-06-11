#include <string.hc>
U8 buf[32];
StrCpy(buf, "hello world");
"%s\n", buf;
StrCpy(buf, "");
"%d\n", StrLen(buf);
