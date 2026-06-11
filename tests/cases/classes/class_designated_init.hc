// class_designated_init.hc — designated initializer for class fields
class Point { I64 x; I64 y; };
Point p = {.x = 5, .y = 9};
"%d %d\n", p.x, p.y;
