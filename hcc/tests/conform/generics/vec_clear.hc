// vec_clear.hc — VecClear resets length but keeps capacity
#include <stdio.hh>
#include <vec.hh>
Vec<I64> v;
VecInit(&v);
VecPush(&v, 1); VecPush(&v, 2); VecPush(&v, 3);
"len=%d cap=%d\n", VecLen(&v), v.cap;
VecClear(&v);
"after clear len=%d\n", VecLen(&v);
VecPush(&v, 42);
"after push len=%d val=%d\n", VecLen(&v), VecAt(&v, 0);
VecFree(&v);
