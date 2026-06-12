// macro_redefine.hc — undef then redefine

#include <stdio.hh>
#define VAL 10
I64 a = VAL;
#undef VAL
#define VAL 20
I64 b = VAL;
#undef VAL
#define VAL 30
I64 c = VAL;
"%d %d %d\n", a, b, c;
