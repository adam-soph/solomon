#include <string.hc>
U8 buf[16];
StrCpy(buf, "ab");
StrNCat(buf, "cdefgh", 3);
"%s\n", buf;
"%d\n", StrLen(buf);
