#include <string.hc>
U8 buf[32];
StrCpy(buf, "hello");
StrCat(buf, " world");
"%s\n", buf;
"%d\n", StrLen(buf);
