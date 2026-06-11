// vec_many_pushes.hc — Vec grows through multiple capacity doublings
#include <vec.hc>
Vec<I64> v;
VecInit(&v);
I64 i;
for (i = 0; i < 64; i++) VecPush(&v, i * 2);
"len=%d\n", VecLen(&v);
"first=%d last=%d\n", VecAt(&v, 0), VecAt(&v, 63);
VecFree(&v);
