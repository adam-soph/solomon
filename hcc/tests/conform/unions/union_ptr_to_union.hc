// union_ptr_to_union.hc — pointer to a union, write and read through it

#include <stdio.hh>
union Val { I64 i; U64 u; };
Val v; v.i = 0;
Val *p = &v;
p->i = 12345;
"%d\n", p->i;
