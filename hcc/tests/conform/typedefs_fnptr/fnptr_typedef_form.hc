// typedef of a function pointer type (anonymous form with name after).

#include <stdio.hh>
typedef I64 (*)(I64, I64) BinOp;
I64 Add(I64 a, I64 b) { return a + b; }
BinOp f = &Add;
"%d\n", f(7, 8);
