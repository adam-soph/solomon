#include <ctype.hc>
U8 *s = "Hello World 123!";
I64 n = 0, i = 0;
while (s[i] != 0) { if (IsAlpha(s[i])) n++; i++; }
"%d\n", n;
