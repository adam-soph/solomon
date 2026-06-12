// vec_clone.hc — VecClone deep-copies a Vec; modifying dst doesn't affect src
#include <stdio.hh>
#include <vec.hh>
Vec<I64> src;
VecInit(&src);
VecPush(&src, 10);
VecPush(&src, 20);
Vec<I64> dst;
VecClone(&dst, &src);
VecPush(&src, 99);
"src=%d dst=%d\n", VecLen(&src), VecLen(&dst);
"dst[0]=%d dst[1]=%d\n", VecAt(&dst,0), VecAt(&dst,1);
VecFree(&src);
VecFree(&dst);
