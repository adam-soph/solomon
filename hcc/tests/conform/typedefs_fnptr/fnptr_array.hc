// Array of function pointers dispatched by index.

#include <stdio.hh>
I64 Add(I64 a, I64 b) { return a + b; }
I64 Sub(I64 a, I64 b) { return a - b; }
I64 Mul(I64 a, I64 b) { return a * b; }

I64 (*ops[3])(I64, I64);
ops[0] = &Add;
ops[1] = &Sub;
ops[2] = &Mul;

I64 i;
for (i = 0; i < 3; i++)
  "%d\n", ops[i](10, 3);
