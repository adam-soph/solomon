#include <stdio.hh>
#include <stdlib.hh>
U8 buf[32];
F64ToStr(1.0, buf);
"%s\n", buf;
F64ToStr(3.14, buf);
"%s\n", buf;
