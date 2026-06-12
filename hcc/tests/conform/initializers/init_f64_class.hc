// Class with F64 fields; initialized positionally.

#include <stdio.hh>
class Vec2 { F64 x; F64 y; };
Vec2 v = {3.0, 4.0};
"%f\n", v.x;
"%f\n", v.y;
