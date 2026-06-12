// inherit_base_zero_init.hc — derived instance; base fields zero-initialized

#include <stdio.hh>
class Base { I64 x; I64 y; };
class Sub : Base { I64 z; };
Sub s;
// x and y are zero (local zero-init), z is zero
"%d %d %d\n", s.x, s.y, s.z;
