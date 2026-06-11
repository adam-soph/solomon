// swap_by_ptr.hc — swap two class fields via pointers
class Box { I64 v; };
U0 Swap(Box *a, Box *b) { I64 t = a->v; a->v = b->v; b->v = t; }
Box x; x.v = 1;
Box y; y.v = 2;
Swap(&x, &y);
"%d %d\n", x.v, y.v;
