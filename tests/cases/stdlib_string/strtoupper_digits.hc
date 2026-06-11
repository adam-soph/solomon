#include <string.hc>
U8 buf[16];
StrCpy(buf, "abc123def");
StrToUpper(buf);
"%s\n", buf;
