// Passing the result of one call to another.
I64 Add(I64 a, I64 b) { return a + b; }
I64 Mul(I64 a, I64 b) { return a * b; }
I64 Neg(I64 a) { return -a; }

"%d\n", Add(Mul(3, 4), Neg(5));
"%d\n", Mul(Add(2, 3), Add(4, 5));
