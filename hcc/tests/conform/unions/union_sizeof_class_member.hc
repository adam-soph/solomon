// union_sizeof_class_member.hc — sizeof union >= sizeof its class member

#include <stdio.hh>
class P { I64 x; I64 y; };
union U { P p; I64 arr[2]; };
// Both are 16 bytes
"%d\n", sizeof(U);
