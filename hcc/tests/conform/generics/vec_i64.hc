// vec_i64.hc — stdlib Vec<I64>: push, at, len, free
#include <stdio.hh>
#include <vec.hh>
Vec<I64> v;
VecInit(&v);
VecPush(&v, 10);
VecPush(&v, 20);
VecPush(&v, 30);
"%d %d %d len=%d\n", VecAt(&v,0), VecAt(&v,1), VecAt(&v,2), VecLen(&v);
VecFree(&v);
