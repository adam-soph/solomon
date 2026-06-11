// shapes.hc — class inheritance, upcasting to a base pointer, tagged dispatch
// via a switch, downcasting, and F64 arithmetic. HolyC has no virtual methods,
// so dispatch is by a `kind` discriminator carried in the shared base.

#define PI 3.14159265358979

#define SHAPE_CIRCLE 0
#define SHAPE_RECT   1
#define SHAPE_TRI    2

class Shape {
  I64 kind;
};

class Circle : Shape {
  F64 radius;
};

class Rect : Shape {
  F64 w;
  F64 h;
};

class Tri : Shape {
  F64 base;
  F64 height;
};

// Each concrete shape is created in-place; a base `Shape *` aliases it, and the
// downcast recovers the full layout (which already contains the derived fields).
F64 Area(Shape *s) {
  switch (s->kind) {
    case SHAPE_CIRCLE: {
      Circle *c = (Circle *)s;
      return PI * c->radius * c->radius;
    }
    case SHAPE_RECT: {
      Rect *r = (Rect *)s;
      return r->w * r->h;
    }
    case SHAPE_TRI: {
      Tri *t = (Tri *)s;
      return 0.5 * t->base * t->height;
    }
  }
  return 0.0;
}

Bool Bigger(Shape *a, Shape *b) {
  return Area(a) > Area(b);
}

U0 Main() {
  Circle c;
  c.kind = SHAPE_CIRCLE;
  c.radius = 2.0;

  Rect r;
  r.kind = SHAPE_RECT;
  r.w = 3.0;
  r.h = 4.0;

  Tri t;
  t.kind = SHAPE_TRI;
  t.base = 6.0;
  t.height = 5.0;

  F64 total = Area(&c) + Area(&r) + Area(&t);
  "rect area = %f\n", Area(&r);
  "tri area = %f\n", Area(&t);
  "total = %f\n", total;
  "rect bigger than tri? %d\n", Bigger(&r, &t);
}

Main;
