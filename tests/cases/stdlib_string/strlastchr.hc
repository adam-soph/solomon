#include <string.hc>
U8 *s = "hello";
U8 *p = StrLastChr(s, 'l');
if (p) "%d\n", p - s;
else "-1\n";
p = StrLastChr(s, 'z');
if (p) "%d\n", p - s;
else "-1\n";
