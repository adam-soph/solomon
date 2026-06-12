// union_array_of_unions.hc — array of unions, each written and read back

#include <stdio.hh>
union Slot { I64 i; U64 u; F64 f; };
Slot arr[3];
arr[0].i = -1;
arr[1].u = 0;
arr[2].f = 2.5;
"%d\n", arr[0].i;
"%d\n", arr[1].u;
"%f\n", arr[2].f;
