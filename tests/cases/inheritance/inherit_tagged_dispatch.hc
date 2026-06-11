// inherit_tagged_dispatch.hc — tagged dispatch over 3 shapes computing areas
#define CIRCLE 0
#define RECT   1
#define TRI    2

class Shape { I64 kind; };
class Circle : Shape { F64 r; };
class Rect   : Shape { F64 w; F64 h; };
class Tri    : Shape { F64 base; F64 ht; };

F64 Area(Shape *s) {
  switch (s->kind) {
    case CIRCLE: { Circle *c = (Circle *)s; return 3.14159265358979 * c->r * c->r; }
    case RECT:   { Rect   *r = (Rect *)s;   return r->w * r->h; }
    case TRI:    { Tri    *t = (Tri *)s;    return 0.5 * t->base * t->ht; }
  }
  return 0.0;
}

Circle c; c.kind = CIRCLE; c.r = 1.0;
Rect   r; r.kind = RECT;   r.w = 4.0; r.h = 3.0;
Tri    t; t.kind = TRI;    t.base = 6.0; t.ht = 4.0;
"%f\n", Area((Shape *)&c);
"%f\n", Area((Shape *)&r);
"%f\n", Area((Shape *)&t);
