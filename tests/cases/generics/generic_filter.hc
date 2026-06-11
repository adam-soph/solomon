// generic_filter.hc — filter elements matching a predicate
#include <vec.hc>
U0 FilterI64(Vec<I64> *src, Vec<I64> *dst, I64 (*pred)(I64)) {
  VecInit(dst);
  I64 i;
  for (i = 0; i < VecLen(src); i++) {
    I64 x = VecAt(src, i);
    if (pred(x)) VecPush(dst, x);
  }
}
I64 IsEven(I64 x) { return x % 2 == 0; }
I64 IsPos(I64 x)  { return x > 0; }

Vec<I64> src, evens, pos;
VecInit(&src);
VecPush(&src, -4); VecPush(&src, 1); VecPush(&src, 2); VecPush(&src, 3); VecPush(&src, 6);
FilterI64(&src, &evens, &IsEven);
FilterI64(&src, &pos, &IsPos);
I64 i;
for (i = 0; i < VecLen(&evens); i++) "%d ", VecAt(&evens, i);
"\n";
for (i = 0; i < VecLen(&pos); i++) "%d ", VecAt(&pos, i);
"\n";
VecFree(&src); VecFree(&evens); VecFree(&pos);
