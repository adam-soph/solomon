// vec_ref_set.hc — VecRef for in-place update, VecSet
#include <stdio.hh>
#include <vec.hh>
Vec<I64> v;
VecInit(&v);
VecPush(&v, 10); VecPush(&v, 20); VecPush(&v, 30);
I64 *ref = VecRef(&v, 1);
*ref = 99;
VecSet(&v, 2, 77);
I64 i;
for (i = 0; i < VecLen(&v); i++) "%d ", VecAt(&v, i);
"\n";
VecFree(&v);
