#include <stdio.hh>
#include <stdlib.hh>
U8 buf[32];
I64ToStr(-12345, buf);
"%s\n", buf;
I64ToStr(-1, buf);
"%s\n", buf;
