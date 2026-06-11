#include <string.hc>
U8 buf[16];
StrCpy(buf, "FooBAR");
StrToLower(buf);
"%s\n", buf;
