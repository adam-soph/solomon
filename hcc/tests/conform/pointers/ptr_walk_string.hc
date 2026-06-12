// Walk a U8* string until the NUL, counting characters.

#include <stdio.hh>
U8 s[6] = {72, 101, 108, 108, 111, 0};
U8 *p = s;
I64 n = 0;
while (*p != 0) { n++; p++; }
"%d\n", n;
