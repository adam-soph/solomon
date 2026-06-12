// multi_instance.hc — computing with several class instances

#include <stdio.hh>
class Vec2 { I64 x; I64 y; };
Vec2 Add(Vec2 a, Vec2 b) { Vec2 r; r.x = a.x + b.x; r.y = a.y + b.y; return r; }
Vec2 a; a.x = 1; a.y = 2;
Vec2 b; b.x = 3; b.y = 4;
Vec2 c; c.x = 5; c.y = 6;
Vec2 s = Add(Add(a, b), c);
"%d %d\n", s.x, s.y;
