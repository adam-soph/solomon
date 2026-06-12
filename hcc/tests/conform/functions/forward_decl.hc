// Forward declaration before use (function defined after its call site).

#include <stdio.hh>
I64 Fwd(I64 x);
I64 Caller(I64 x) { return Fwd(x) + 1; }
I64 Fwd(I64 x) { return x * 3; }
"%d\n", Caller(4);
"%d\n", Caller(7);
