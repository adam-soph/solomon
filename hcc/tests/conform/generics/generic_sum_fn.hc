// generic_sum_fn.hc — Sum with type dispatch for I64 and F64
#include <stdio.hh>
#include <vec.hh>
I64 SumI64Vec(Vec<I64> *v) { I64 s=0,i; for(i=0;i<VecLen(v);i++) s+=VecAt(v,i); return s; }
F64 SumF64Vec(Vec<F64> *v) { F64 s=0.0; I64 i; for(i=0;i<VecLen(v);i++) s+=VecAt(v,i); return s; }

Vec<I64> vi;
VecInit(&vi);
VecPush(&vi, 1); VecPush(&vi, 2); VecPush(&vi, 3); VecPush(&vi, 4); VecPush(&vi, 5);
"%d\n", SumI64Vec(&vi);
VecFree(&vi);

Vec<F64> vf;
VecInit(&vf);
VecPush(&vf, 0.1); VecPush(&vf, 0.2); VecPush(&vf, 0.3);
"%.1f\n", SumF64Vec(&vf);
VecFree(&vf);
