// vec_pop.hc — VecPop drains stack-style
#include <vec.hc>
Vec<I64> v;
VecInit(&v);
VecPush(&v, 100); VecPush(&v, 200); VecPush(&v, 300);
I64 x;
x = VecPop(&v); "%d\n", x;
x = VecPop(&v); "%d\n", x;
x = VecPop(&v); "%d\n", x;
"empty=%d\n", VecLen(&v) == 0;
VecFree(&v);
