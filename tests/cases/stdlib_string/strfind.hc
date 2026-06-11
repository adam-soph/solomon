#include <string.hc>
U8 *hay = "hello world";
U8 *p = StrFind(hay, "world");
if (p) "%s\n", p;
else "not found\n";
p = StrFind(hay, "xyz");
if (p) "%s\n", p;
else "not found\n";
