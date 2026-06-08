// generic.hc — monomorphized generics, end to end: generic classes and functions
// with inferred type arguments, the three parameter kinds (`type` / `int` /
// `comparable`), and a compile-time type switch. Each use with concrete arguments is
// stamped out and type-checked at compile time, so you get typed APIs with no casts —
// one template serves every element type.

#include <stdlib.hc>   // ReAlloc

// ---- generic classes + functions, with inferred type arguments ----

class Vec<type T> { T *data; I64 len; I64 cap; }

U0 VecInit<type T>(Vec<T> *v) { v->data = NULL; v->len = 0; v->cap = 0; }

U0 VecPush<type T>(Vec<T> *v, T x)
{
  if (v->len >= v->cap) {
    I64 c = v->cap ? v->cap * 2 : 4;
    v->data = ReAlloc(v->data, v->cap * sizeof(T), c * sizeof(T));
    v->cap = c;
  }
  v->data[v->len++] = x;
}

T VecAt<type T>(Vec<T> *v, I64 i) { return v->data[i]; }
U0 VecFree<type T>(Vec<T> *v) { if (v->data) Free(v->data); }

// ---- parameter kinds ----
//   * `type T`       — a type parameter (the explicit spelling of a bare `<T>`).
//   * `int N`        — a value parameter: a compile-time integer, e.g. an array size.
//   * `comparable T` — a type parameter constrained to orderable types (a scalar or
//                      pointer), so the body may use `<` / `>`.

// A fixed-capacity array: `int N` is the compile-time capacity, `type T` the element type.
class FixedArr<type T, int N> {
  T data[N];
  I64 len;
}

U0 FAInit<type T, int N>(FixedArr<T, N> *a) { a->len = 0; }
U0 FAPush<type T, int N>(FixedArr<T, N> *a, T x) { a->data[a->len++] = x; }
T FAAt<type T, int N>(FixedArr<T, N> *a, I64 i) { return a->data[i]; }

// `comparable T` lets the body order `T` values. Instantiating with a non-orderable
// type (a class) would be a compile-time error.
T Max<comparable T>(T a, T b) { return a > b ? a : b; }

// ---- compile-time type test (`if type`), the analogue of Go's `switch v.(type)` ----
// Resolved per instantiation: only the branch matching the concrete `T` survives; the
// rest are discarded before type-checking. `if type (T is U)` is the single-case form
// (chain with `else`); `switch type` is the multi-way form.
U0 Show<type T>(T x) {
  if type (T is I64)        "I64 %d\n", x;
  else if type (T is F64)   "F64 %.2f\n", x;
  else if type (T is U8 *)  "str %s\n", x;
  else                      "other\n";
}

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

  // `int N` capacity. The array field `T data[N]` becomes `I64 data[8]`.
  FixedArr<I64, 8> xs;
  FAInit<I64, 8>(&xs);
  for (i = 0; i < 5; i++) FAPush<I64, 8>(&xs, i * i);
  "sizeof = %d\n", sizeof(FixedArr<I64, 8>); // 8*8 + 8 = 72
  "len=%d first=%d last=%d\n", xs.len, FAAt<I64, 8>(&xs, 0), FAAt<I64, 8>(&xs, 4);

  // A different N (and T) is a distinct, independent type.
  FixedArr<U8 *, 2> ss;
  FAInit<U8 *, 2>(&ss);
  FAPush<U8 *, 2>(&ss, "hello");
  FAPush<U8 *, 2>(&ss, "world");
  "%s %s\n", FAAt<U8 *, 2>(&ss, 0), FAAt<U8 *, 2>(&ss, 1);

  // `comparable T`, ordered both as I64 and F64.
  "max i = %d\n", Max(3, 9);
  "max f = %.1f\n", Max(2.5, 1.5);

  // The compile-time type switch, dispatched per instantiation.
  Show(42);
  Show(3.14);
  Show("hi");
}

Main;
