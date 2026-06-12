// class_brace_init.hc — brace initializer for a class

#include <stdio.hh>
class Point { I64 x; I64 y; };
Point p = {3, 7};
"%d %d\n", p.x, p.y;
