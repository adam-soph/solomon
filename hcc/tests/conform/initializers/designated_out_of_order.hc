// Designated initializer with fields specified out of declaration order.

#include <stdio.hh>
class Pt { I64 x; I64 y; };
Pt p = {.y = 7, .x = 3};
"%d %d\n", p.x, p.y;
