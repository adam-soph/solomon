// tuple_in_fn_arg.hc — pass a tuple variable as fn argument

#include <stdio.hh>
typedef (I64, I64) Pair;
I64 Sum(Pair p) { return p[0] + p[1]; }
I64 Product(Pair p) { return p[0] * p[1]; }

Pair p = (6, 7);
"%d %d\n", Sum(p), Product(p);
