// Global function pointer assigned and called.
typedef I64 (*)(I64, I64) BinOp;
BinOp g_op = NULL;

I64 Add(I64 a, I64 b) { return a + b; }
I64 Mul(I64 a, I64 b) { return a * b; }

U0 Main() {
  g_op = &Add;
  "%d\n", g_op(3, 4);
  g_op = &Mul;
  "%d\n", g_op(3, 4);
}
Main;
