// Narrow integer types truncate at the declared width (C rules).

#include <stdio.hh>
U8 a = 300;        // 300 & 0xFF == 44
I8 b = 200;        // wraps to -56
U16 c = 70000;     // 70000 & 0xFFFF == 4464
"%d %d %d\n", a, b, c;
