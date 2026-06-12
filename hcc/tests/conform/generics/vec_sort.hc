// vec_sort.hc — VecSort with CmpI64
#include <stdio.hh>
#include <stdlib.hh>
#include <vec.hh>
Vec<I64> v;
VecInit(&v);
VecPush(&v, 5);
VecPush(&v, 2);
VecPush(&v, 8);
VecPush(&v, 1);
VecPush(&v, 4);
VecSort(&v, &CmpI64);
I64 i;
for (i = 0; i < VecLen(&v); i++)
  "%d ", VecAt(&v, i);
"\n";
VecFree(&v);
