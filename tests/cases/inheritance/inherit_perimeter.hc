// inherit_perimeter.hc — compute perimeter for shape hierarchy
#define R_KIND 0
#define C_KIND 1

class Shape { I64 kind; };
class Rect  : Shape { F64 w; F64 h; };
class Circ  : Shape { F64 r; };

F64 Perim(Shape *s) {
  switch (s->kind) {
    case R_KIND: { Rect *r = (Rect *)s; return 2.0 * (r->w + r->h); }
    case C_KIND: { Circ *c = (Circ *)s; return 2.0 * 3.14159265358979 * c->r; }
  }
  return 0.0;
}

Rect rect; rect.kind = R_KIND; rect.w = 5.0; rect.h = 3.0;
Circ circ; circ.kind = C_KIND; circ.r = 2.0;
"%f\n", Perim((Shape *)&rect);
"%f\n", Perim((Shape *)&circ);
