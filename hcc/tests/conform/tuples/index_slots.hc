// index_slots.hc — index a tuple variable with literal k

#include <stdio.hh>
(I64, F64, U8 *) Make() { return 10, 3.14, "hi"; }
(I64, F64, U8 *) t = Make();
"%d\n", t[0];
"%.2f\n", t[1];
"%s\n", t[2];
