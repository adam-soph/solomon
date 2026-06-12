// Designated init then field mutation.

#include <stdio.hh>
class Pt { I64 x; I64 y; };
Pt p = {.x = 1, .y = 2};
p.x += 10;
p.y *= 3;
"%d %d\n", p.x, p.y;
