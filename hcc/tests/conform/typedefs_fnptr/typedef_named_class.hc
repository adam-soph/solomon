// typedef of a named class.

#include <stdio.hh>
class Point { I64 x; I64 y; };
typedef Point Pt;
Pt p;
p.x = 10;
p.y = 20;
"%d %d\n", p.x, p.y;
