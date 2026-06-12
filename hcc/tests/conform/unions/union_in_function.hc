// union_in_function.hc — union local inside a function

#include <stdio.hh>
#include <stdlib.hh>
U0 Show(I64 v) {
  union { I64 i; U64 u; } x;
  x.i = v;
  "%d %d\n", x.i, (x.u == (U64)v) ? 1 : 0;
}
Show(7);
Show(-1);
