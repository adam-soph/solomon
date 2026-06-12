// inherit_three_shapes.hc — three shapes, each tested with Area

#include <stdio.hh>
class Sh { I64 kind; };
class Sq : Sh { I64 side; };
class Ob : Sh { I64 w; I64 h; };

I64 Area(Sh *s) {
  switch (s->kind) {
    case 0: return ((Sq *)s)->side * ((Sq *)s)->side;
    case 1: return ((Ob *)s)->w * ((Ob *)s)->h;
  }
  return 0;
}

Sq sq; sq.kind = 0; sq.side = 5;
Ob ob; ob.kind = 1; ob.w = 4; ob.h = 6;
"%d %d\n", Area((Sh *)&sq), Area((Sh *)&ob);
