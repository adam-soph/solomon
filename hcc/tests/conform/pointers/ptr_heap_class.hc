// Heap-allocate a class, write fields via ->, read back.

#include <stdio.hh>
class Rect { I64 w; I64 h; };

Rect *r = MAlloc(sizeof(Rect));
r->w = 6;
r->h = 9;
"%d %d area=%d\n", r->w, r->h, r->w * r->h;
Free(r);
