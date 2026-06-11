// vec_bsearch.hc — VecBSearch on a sorted Vec
#include <vec.hc>
#include <stdlib.hc>
Vec<I64> v;
VecInit(&v);
VecPush(&v, 10); VecPush(&v, 30); VecPush(&v, 50); VecPush(&v, 70); VecPush(&v, 90);
// already sorted
I64 key = 50;
I64 idx = VecBSearch(&v, &key, &CmpI64);
"found 50 at idx=%d\n", idx;
key = 20;
idx = VecBSearch(&v, &key, &CmpI64);
"found 20 at idx=%d\n", idx;
VecFree(&v);
