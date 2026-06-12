// field_via_ptr.hc — field access via ->

#include <stdio.hh>
class Point { I64 x; I64 y; };
Point p;
p.x = 3;
p.y = 4;
Point *pp = &p;
pp->x = pp->x + 1;
pp->y = pp->y * 2;
"%d %d\n", p.x, p.y;
