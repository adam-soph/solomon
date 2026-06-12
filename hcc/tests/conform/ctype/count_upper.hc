
#include <ctype.hh>
#include <stdio.hh>
U8 *s = "Hello WORLD foo";
I64 n = 0, i = 0;
while (s[i] != 0) { if (IsUpper(s[i])) n++; i++; }
"%d\n", n;
