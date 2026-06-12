// Array of classes; iterate with a class* pointer using ->.

#include <stdio.hh>
class Pt { I64 x; I64 y; };

Pt pts[3];
pts[0].x = 1; pts[0].y = 2;
pts[1].x = 3; pts[1].y = 4;
pts[2].x = 5; pts[2].y = 6;

Pt *p = pts;
I64 i;
for (i = 0; i < 3; i++) {
  "%d,%d ", p->x, p->y;
  p++;
}
"\n";
