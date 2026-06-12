// union_swap_via_largest.hc — swap two unions by copying the largest field

#include <stdio.hh>
union W { I64 a; I64 b; };
W x; x.a = 10; W y; y.a = 20;
W tmp; tmp.a = x.a; x.a = y.a; y.a = tmp.a;
"%d %d\n", x.a, y.a;
