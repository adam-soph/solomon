#include <string.hc>
U8 hay[8]; U8 needle[4];
hay[0] = 'a'; hay[1] = 'b'; hay[2] = 'c'; hay[3] = 'd'; hay[4] = 'e';
needle[0] = 'c'; needle[1] = 'd'; needle[2] = 0;
U8 *p = MemSearch(hay, 5, needle, 2);
if (p) "%d\n", p - hay;
else "-1\n";
needle[0] = 'z'; needle[1] = 0;
p = MemSearch(hay, 5, needle, 1);
if (p) "%d\n", p - hay;
else "-1\n";
