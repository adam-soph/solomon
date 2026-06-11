// member_access_on_call.hc — member access on a call result
class Point { I64 x; I64 y; };
Point Mk(I64 x, I64 y) { Point p; p.x = x; p.y = y; return p; }
I64 vx = Mk(5, 7).x;
I64 vy = Mk(3, 8).y;
"%d %d\n", vx, vy;
