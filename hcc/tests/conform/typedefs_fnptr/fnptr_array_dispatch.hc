// Dispatch by index through an array of fn-ptrs; also exercises NULL check.

#include <stdio.hh>
I64 Inc(I64 x) { return x + 1; }
I64 Dec(I64 x) { return x - 1; }
I64 Neg(I64 x) { return -x; }
I64 Dbl(I64 x) { return x * 2; }

I64 (*ops[4])(I64);
ops[0] = &Inc;
ops[1] = &Dec;
ops[2] = &Neg;
ops[3] = &Dbl;

I64 val = 5, i;
for (i = 0; i < 4; i++) {
  val = ops[i](val);
  "%d\n", val;
}
