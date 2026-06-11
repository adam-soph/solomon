// Class containing an array of another class.
class Pt { I64 x; I64 y; };
class Triangle { Pt pts[3]; };
Triangle t;
t.pts[0].x = 0; t.pts[0].y = 0;
t.pts[1].x = 1; t.pts[1].y = 0;
t.pts[2].x = 0; t.pts[2].y = 1;
I64 i;
for (i = 0; i < 3; i++)
  "%d %d\n", t.pts[i].x, t.pts[i].y;
