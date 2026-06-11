// Nested designated initializer.
class Pt { I64 x; I64 y; };
class Line { Pt a; Pt b; I64 tag; };
Line l = {.tag = 42, .b = {.x = 5, .y = 6}};
"%d %d %d %d %d\n", l.a.x, l.a.y, l.b.x, l.b.y, l.tag;
