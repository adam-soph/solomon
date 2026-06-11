#include <string.hc>
U8 buf[16];
StrNCpy(buf, "hello", 3);
buf[3] = 0;
"%s\n", buf;
StrNCpy(buf, "hi", 8);
"%s\n", buf;
