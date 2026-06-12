// Function pointer as a class field (tiny vtable).

#include <stdio.hh>
class Ops {
  I64 (*apply)(I64, I64);
};

I64 Add(I64 a, I64 b) { return a + b; }
I64 Mul(I64 a, I64 b) { return a * b; }

Ops adder;
adder.apply = &Add;
Ops muler;
muler.apply = &Mul;

"%d\n", adder.apply(5, 3);
"%d\n", muler.apply(5, 3);
