// generic_reduce.hc — fold/reduce using a function pointer
#include <stdio.hh>
#include <vec.hh>
I64 ReduceI64(Vec<I64> *v, I64 init, I64 (*f)(I64, I64)) {
  I64 acc = init, i;
  for (i = 0; i < VecLen(v); i++) acc = f(acc, VecAt(v, i));
  return acc;
}
I64 Add(I64 a, I64 b) { return a + b; }
I64 Mul(I64 a, I64 b) { return a * b; }

Vec<I64> v;
VecInit(&v);
VecPush(&v, 2); VecPush(&v, 3); VecPush(&v, 4); VecPush(&v, 5);
"%d\n", ReduceI64(&v, 0, &Add);
"%d\n", ReduceI64(&v, 1, &Mul);
VecFree(&v);
