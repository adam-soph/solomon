// tuple_in_vec.hc — tuples as Vec elements (pass tuple variables)
#include <vec.hc>
Vec<(I64, I64)> v;
VecInit(&v);
(I64, I64) e0 = (1, 10);
(I64, I64) e1 = (2, 20);
(I64, I64) e2 = (3, 30);
VecPush(&v, e0);
VecPush(&v, e1);
VecPush(&v, e2);
I64 i;
for (i = 0; i < VecLen(&v); i++) {
  (I64, I64) t = VecAt(&v, i);
  "%d->%d\n", t[0], t[1];
}
VecFree(&v);
