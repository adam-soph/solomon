#include <stdio.hh>
#include <stdlib.hh>
#include <vec.hh>
class Pt { I64 x; I64 y; }
U0 Main() {
  Vec<I64> v; VecInit(&v);
  I64 i;
  for (i = 0; i < 10; i++) VecPush(&v, i * i);
  "len=%d capok=%d at5=%d\n", VecLen(&v), v.cap >= v.len, VecAt(&v, 5);
  "pop=%d pop=%d len=%d\n", VecPop(&v), VecPop(&v), VecLen(&v);

  VecSet(&v, 0, 99);
  Vec<I64> c; VecClone(&c, &v);
  VecPush(&c, 7);
  "clone0=%d clen=%d vlen=%d\n", VecAt(&c, 0), VecLen(&c), VecLen(&v);

  Vec<F64> f; VecInit(&f);            // F64 elements
  VecPush(&f, 1.5);
  VecPush(&f, 2.5);
  "f64 %.1f %.1f\n", VecAt(&f, 0), VecAt(&f, 1);

  Vec<U8 *> s; VecInit(&s);           // pointer elements
  VecPush(&s, "a");
  VecPush(&s, "b");
  Vec<U8 *> sc; VecClone(&sc, &s);    // clone keeps the pointers valid
  "ptr %s %s\n", VecAt(&s, 0), VecAt(&sc, 1);

  Vec<Pt> p; VecInit(&p);             // class values
  Pt e; e.x = 1; e.y = 2; VecPush(&p, e);
  e.x = 3; e.y = 4; VecPush(&p, e);
  Pt g = VecAt(&p, 1);                // load a whole class by value
  "class %d %d\n", g.x, g.y;

  VecFree(&v); VecFree(&c); VecFree(&f); VecFree(&s); VecFree(&sc); VecFree(&p);
}
Main;
