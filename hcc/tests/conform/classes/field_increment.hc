// field_increment.hc — increment fields with ++ and +=

#include <stdio.hh>
class Cnt { I64 a; I64 b; };
Cnt c; c.a = 0; c.b = 10;
c.a++;
c.a++;
c.b += 5;
"%d %d\n", c.a, c.b;
