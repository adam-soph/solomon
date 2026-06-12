// union_overlap.hc — write one variant, read another, show overlap

#include <stdio.hh>
union Both { U64 u; I64 s; };
Both b; b.u = 0xFFFFFFFFFFFFFFFF;
// reading as signed gives -1
"%d\n", b.s;
