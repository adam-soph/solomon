// two_ptrs_same_obj.hc — two pointers to the same object; write through one, see via other

#include <stdio.hh>
class Box { I64 v; };
Box b; b.v = 0;
Box *p = &b;
Box *q = &b;
p->v = 55;
"%d\n", q->v;
