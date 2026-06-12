// map_fn_generic.hc — generic map: apply fn to each element
#include <stdio.hh>
#include <stdlib.hh>
#include <vec.hh>
U0 MapI64(Vec<I64> *v, I64 (*f)(I64)) {
  I64 i;
  for (i = 0; i < VecLen(v); i++)
    VecSet(v, i, f(VecAt(v, i)));
}
I64 Double(I64 x) { return x * 2; }
I64 Square(I64 x) { return x * x; }

Vec<I64> v;
VecInit(&v);
VecPush(&v, 1); VecPush(&v, 2); VecPush(&v, 3); VecPush(&v, 4);
MapI64(&v, &Double);
I64 i;
for (i = 0; i < VecLen(&v); i++) "%d ", VecAt(&v, i);
"\n";
MapI64(&v, &Square);
for (i = 0; i < VecLen(&v); i++) "%d ", VecAt(&v, i);
"\n";
VecFree(&v);
