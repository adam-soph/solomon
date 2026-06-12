// union_multi_field_class.hc — union of two different-sized classes

#include <stdio.hh>
class Small { I64 x; };
class Big { I64 a; I64 b; I64 c; };
union Either { Small s; Big g; };
Either e; e.g.a = 1; e.g.b = 2; e.g.c = 3;
"%d %d\n", e.s.x, e.g.b;
