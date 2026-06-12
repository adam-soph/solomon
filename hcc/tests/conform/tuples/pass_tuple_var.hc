// pass_tuple_var.hc — pass a tuple variable through a function

#include <stdio.hh>
typedef (I64, I64) Pair;
Pair Swap(Pair p) { return p[1], p[0]; }
Pair Add1(Pair p) { return p[0]+1, p[1]+1; }

Pair orig = (3, 7);
Pair sw = Swap(orig);
"%d %d\n", sw[0], sw[1];
Pair inc = Add1(orig);
"%d %d\n", inc[0], inc[1];
