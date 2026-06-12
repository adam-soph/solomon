// Basic function pointer declared keyword-less, initialized on declaration.

#include <stdio.hh>
I64 Add(I64 a, I64 b) { return a + b; }
I64 (*op)(I64, I64) = &Add;
"%d\n", op(3, 4);
