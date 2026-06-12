// A function's own local `i` shadows the top-level `i`; the two are independent — the
// top-level counter is unaffected by the function's loop.

#include <stdio.hh>
I64 i = 100;
I64 SumTo(I64 n) { I64 i, s = 0; for (i = 0; i < n; i++) s += i; return s; }
"top i=%d\n", i;
"sum=%d\n", SumTo(4);
"top i=%d\n", i;
