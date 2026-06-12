
#include <stdio.hh>
#include <string.hh>
U8 *s = "hello";
// StrChr with c==0 finds the NUL terminator
U8 *p = StrChr(s, 0);
if (p) "%d\n", p - s;
else "-1\n";
