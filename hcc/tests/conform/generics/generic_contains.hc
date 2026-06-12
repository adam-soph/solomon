// generic_contains.hc — generic linear search
#include <stdio.hh>
#include <vec.hh>
I64 ContainsI64(Vec<I64> *v, I64 x) {
  I64 i;
  for (i = 0; i < VecLen(v); i++)
    if (VecAt(v, i) == x) return 1;
  return 0;
}
Vec<I64> v;
VecInit(&v);
VecPush(&v, 10); VecPush(&v, 20); VecPush(&v, 30);
"%d\n", ContainsI64(&v, 20);
"%d\n", ContainsI64(&v, 99);
VecFree(&v);
