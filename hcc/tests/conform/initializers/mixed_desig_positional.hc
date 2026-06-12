// Mix designated and partial; rely on zeroing for unspecified fields.

#include <stdio.hh>
class Tri { I64 a; I64 b; I64 c; };
Tri t = {.c = 9, .a = 1};
"%d %d %d\n", t.a, t.b, t.c;
