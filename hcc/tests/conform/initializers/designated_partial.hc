// Designated partial: unspecified field defaults to 0.

#include <stdio.hh>
class Pt { I64 x; I64 y; };
Pt p = {.y = 9};
"%d %d\n", p.x, p.y;
