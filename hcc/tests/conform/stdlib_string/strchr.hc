
#include <stdio.hh>
#include <string.hh>
U8 *s = "hello";
U8 *p = StrChr(s, 'l');
if (p) "%d\n", p - s;
else "-1\n";
p = StrChr(s, 'z');
if (p) "%d\n", p - s;
else "-1\n";
