// field_read_write.hc — basic field read and write by value
class Point { I64 x; I64 y; };
Point p;
p.x = 7;
p.y = 13;
"%d %d\n", p.x, p.y;
