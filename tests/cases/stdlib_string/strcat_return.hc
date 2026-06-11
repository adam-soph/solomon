#include <string.hc>
U8 buf[32];
StrCpy(buf, "foo");
U8 *r = StrCat(buf, "bar");
"%s\n", r;
