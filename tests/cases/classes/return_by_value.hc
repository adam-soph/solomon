// return_by_value.hc — class returned by value from a function
class Point { I64 x; I64 y; };
Point Make(I64 x, I64 y) { Point p; p.x = x; p.y = y; return p; }
Point q = Make(6, 9);
"%d %d\n", q.x, q.y;
