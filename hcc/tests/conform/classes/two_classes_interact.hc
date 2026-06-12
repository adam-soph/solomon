// two_classes_interact.hc — two different class types interact through functions

#include <stdio.hh>
class Vec2 { I64 x; I64 y; };
class Seg { Vec2 a; Vec2 b; };
I64 ManhLen(Seg s) {
  I64 dx = s.b.x - s.a.x;
  I64 dy = s.b.y - s.a.y;
  if (dx < 0) dx = -dx;
  if (dy < 0) dy = -dy;
  return dx + dy;
}
Seg seg;
seg.a.x = 1; seg.a.y = 2;
seg.b.x = 5; seg.b.y = 7;
"%d\n", ManhLen(seg);
