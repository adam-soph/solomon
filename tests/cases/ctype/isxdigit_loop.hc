#include <ctype.hc>
U8 *s = "0123456789abcdefABCDEFghij";
I64 n = 0, i = 0;
while (s[i] != 0) { if (IsXDigit(s[i])) n++; i++; }
"%d\n", n;
