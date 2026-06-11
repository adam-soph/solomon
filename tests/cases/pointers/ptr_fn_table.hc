// Function pointer stored in a class field (vtable pattern).
class Op {
  I64 a;
  I64 b;
  I64 (*fn)(I64, I64);
};

I64 Add(I64 a, I64 b) { return a + b; }
I64 Mul(I64 a, I64 b) { return a * b; }

Op op;
op.a = 3; op.b = 4;
op.fn = &Add;
"%d\n", op.fn(op.a, op.b);
op.fn = &Mul;
"%d\n", op.fn(op.a, op.b);
