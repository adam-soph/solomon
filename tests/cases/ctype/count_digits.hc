#include <ctype.hc>
U8 *s = "abc123def456";
I64 n = 0, i = 0;
while (s[i] != 0) { if (IsDigit(s[i])) n++; i++; }
"%d\n", n;
