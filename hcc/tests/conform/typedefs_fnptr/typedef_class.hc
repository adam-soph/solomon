// typedef of a class (anonymous form).

#include <stdio.hh>
typedef class { I64 x; I64 y; } Box;
Box b;
b.x = 3;
b.y = 4;
"%d %d\n", b.x, b.y;
