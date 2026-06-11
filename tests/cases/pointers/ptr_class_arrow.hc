// Pointer to a class and -> member access.
class Point {
  I64 x;
  I64 y;
};

Point pt;
pt.x = 3;
pt.y = 7;
Point *p = &pt;
p->x = p->x + 10;
"%d %d\n", p->x, p->y;
