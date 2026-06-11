// zero_init.hc — locals are zero-initialized
class Point { I64 x; I64 y; };
Point p;
"%d %d\n", p.x, p.y;
