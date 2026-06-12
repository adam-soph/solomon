// class_swap_value.hc — swap two class instances by value using a temp

#include <stdio.hh>
class Box { I64 v; };
Box a; a.v = 11;
Box b; b.v = 22;
Box tmp = a; a = b; b = tmp;
"%d %d\n", a.v, b.v;
