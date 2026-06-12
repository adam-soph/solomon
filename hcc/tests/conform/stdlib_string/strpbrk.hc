
#include <stdio.hh>
#include <string.hh>
U8 *s = "hello world";
U8 *p = StrPBrk(s, "aeiou");
if (p) "%d\n", p - s;
else "-1\n";
p = StrPBrk(s, "xyz");
if (p) "%d\n", p - s;
else "-1\n";
