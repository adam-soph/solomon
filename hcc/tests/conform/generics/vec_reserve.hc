// vec_reserve.hc — VecReserve pre-allocates capacity
#include <stdio.hh>
#include <vec.hh>
Vec<I64> v;
VecInit(&v);
VecReserve(&v, 100);
"cap_after_reserve >= 100: %d\n", v.cap >= 100;
I64 i;
for (i = 0; i < 10; i++) VecPush(&v, i * 3);
"len=%d\n", VecLen(&v);
"val[9]=%d\n", VecAt(&v, 9);
VecFree(&v);
