
#include <ctype.hh>
#include <stdio.hh>
U8 *s = "a b\tc\nd";
I64 n = 0, i = 0;
while (s[i] != 0) { if (IsSpace(s[i])) n++; i++; }
"%d\n", n;
