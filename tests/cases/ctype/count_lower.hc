#include <ctype.hc>
U8 *s = "Hello World";
I64 n = 0, i = 0;
while (s[i] != 0) { if (IsLower(s[i])) n++; i++; }
"%d\n", n;
