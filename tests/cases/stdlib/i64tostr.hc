#include <stdlib.hc>
U8 buf[32];
I64ToStr(12345, buf);
"%s\n", buf;
I64ToStr(-99, buf);
"%s\n", buf;
I64ToStr(0, buf);
"%s\n", buf;
