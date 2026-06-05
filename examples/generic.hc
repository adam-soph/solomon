// generic.hc — monomorphized generics: generic classes (`class Vec<T>`) and generic
// functions (`VecPush<T>(...)`). Each use with a concrete type is stamped out and
// type-checked at compile time, so you get typed APIs with no casts — one template
// serves every element type.

#include <mem.hc>

class Vec<T> { T *data; I64 len; I64 cap; }

U0 VecInit<T>(Vec<T> *v) { v->data = NULL; v->len = 0; v->cap = 0; }

U0 VecPush<T>(Vec<T> *v, T x)
{
  if (v->len >= v->cap) {
    I64 c = v->cap ? v->cap * 2 : 4;
    v->data = ReAlloc(v->data, v->cap * sizeof(T), c * sizeof(T));
    v->cap = c;
  }
  v->data[v->len++] = x;
}

T VecAt<T>(Vec<T> *v, I64 i) { return v->data[i]; }
U0 VecFree<T>(Vec<T> *v) { if (v->data) Free(v->data); }

T Max<T>(T a, T b) { return a > b ? a : b; }   // a free generic function

U0 Main()
{
  // A typed I64 vector. The type argument is **inferred** from the arguments —
  // `VecPush(&v, 10)` figures out `T = I64` from `&v` (a `Vec<I64>*`). (Explicit
  // `VecPush<I64>(&v, 10)` works too.)
  Vec<I64> v;
  VecInit(&v);
  VecPush(&v, 10);
  VecPush(&v, 20);
  VecPush(&v, 30);
  I64 i, hi = VecAt(&v, 0);
  for (i = 1; i < v.len; i++) hi = Max(hi, VecAt(&v, i));
  "ints: len=%d max=%d\n", v.len, hi;
  VecFree(&v);

  // The same templates, inferred as F64 here.
  Vec<F64> f;
  VecInit(&f);
  VecPush(&f, 1.5);
  VecPush(&f, 2.5);
  "flts: %.1f + %.1f = %.1f\n",
      VecAt(&f, 0), VecAt(&f, 1), VecAt(&f, 0) + VecAt(&f, 1);
  VecFree(&f);
}

Main;
