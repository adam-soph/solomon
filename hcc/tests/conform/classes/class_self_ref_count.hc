// class_self_ref_count.hc — self-referential class; count nodes in a chain

#include <stdio.hh>
class Link { I64 v; Link *nx; };
Link a; a.v = 1; a.nx = NULL;
Link b; b.v = 2; b.nx = &a;
Link c; c.v = 3; c.nx = &b;
I64 count = 0;
Link *p = &c;
while (p != NULL) { count++; p = p->nx; }
"%d\n", count;
