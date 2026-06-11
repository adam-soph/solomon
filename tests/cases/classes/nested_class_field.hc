// nested_class_field.hc — nested class as a field
class Point { I64 x; I64 y; };
class Rect { Point lo; Point hi; };
Rect r;
r.lo.x = 1; r.lo.y = 2;
r.hi.x = 5; r.hi.y = 7;
I64 w = r.hi.x - r.lo.x;
I64 h = r.hi.y - r.lo.y;
"%d %d\n", w, h;
