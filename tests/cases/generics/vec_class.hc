// vec_class.hc — Vec<Point>: class elements pushed and retrieved
#include <vec.hc>
class Point { I64 x; I64 y; };
Vec<Point> v;
VecInit(&v);
Point p;
p.x = 1; p.y = 2; VecPush(&v, p);
p.x = 3; p.y = 4; VecPush(&v, p);
p.x = 5; p.y = 6; VecPush(&v, p);
I64 i;
for (i = 0; i < VecLen(&v); i++) {
  Point q = VecAt(&v, i);
  "(%d,%d)\n", q.x, q.y;
}
VecFree(&v);
