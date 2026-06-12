// Designated init for a class with F64 fields.

#include <stdio.hh>
class V { I64 n; F64 x; };
V v = {.x = 2.5, .n = 3};
"%d %f\n", v.n, v.x;
