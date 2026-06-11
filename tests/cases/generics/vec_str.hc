// vec_str.hc — Vec<U8*>: string pointer elements
#include <vec.hc>
Vec<U8 *> v;
VecInit(&v);
VecPush(&v, "alpha");
VecPush(&v, "beta");
VecPush(&v, "gamma");
I64 i;
for (i = 0; i < VecLen(&v); i++)
  "%s\n", VecAt(&v, i);
VecFree(&v);
