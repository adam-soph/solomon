// whole_class_assign.hc — whole-class assignment is a deep copy

#include <stdio.hh>
class Point { I64 x; I64 y; };
Point a; a.x = 11; a.y = 22;
Point b = a;
b.x = 99;
"%d %d\n", a.x, a.y;   // a unchanged
"%d %d\n", b.x, b.y;
