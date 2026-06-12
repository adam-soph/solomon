// class_as_return_computed.hc — return value computed from two inputs

#include <stdio.hh>
class Vec2 { I64 x; I64 y; };
Vec2 Scale(Vec2 v, I64 s) { Vec2 r; r.x = v.x * s; r.y = v.y * s; return r; }
Vec2 v; v.x = 3; v.y = 4;
Vec2 w = Scale(v, 3);
"%d %d\n", w.x, w.y;
