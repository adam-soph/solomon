// Function pointer: store address, call through it.

#include <stdio.hh>
I64 Square(I64 x) { return x * x; }
I64 Double(I64 x) { return x * 2; }

typedef I64 (*)(I64) MathFn;
MathFn f = &Square;
"%d\n", f(5);
f = &Double;
"%d\n", f(5);
