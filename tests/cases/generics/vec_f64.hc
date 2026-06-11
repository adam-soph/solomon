// vec_f64.hc — stdlib Vec<F64>: push, at, pop
#include <vec.hc>
Vec<F64> v;
VecInit(&v);
VecPush(&v, 1.5);
VecPush(&v, 2.5);
VecPush(&v, 3.5);
"len=%d sum=%.1f\n", VecLen(&v), VecAt(&v,0)+VecAt(&v,1)+VecAt(&v,2);
F64 p = VecPop(&v);
"pop=%.1f len=%d\n", p, VecLen(&v);
VecFree(&v);
