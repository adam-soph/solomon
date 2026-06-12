// Inline expressions as call arguments.

#include <stdio.hh>
I64 F(I64 x) { return x * x; }
"%d\n", F(2 + 3);
"%d\n", F(F(2));
I64 a = 3;
"%d\n", F(a++);
"%d\n", a;
