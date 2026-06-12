// array_field_sum.hc — class with an array field summed

#include <stdio.hh>
class Vec3 { I64 v[3]; };
Vec3 u;
u.v[0] = 10; u.v[1] = 20; u.v[2] = 30;
I64 s = u.v[0] + u.v[1] + u.v[2];
"%d\n", s;
