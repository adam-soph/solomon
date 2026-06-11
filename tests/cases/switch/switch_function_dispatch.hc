// switch used as a dispatch table via function calls.
I64 Add(I64 a, I64 b) { return a + b; }
I64 Sub(I64 a, I64 b) { return a - b; }
I64 Mul(I64 a, I64 b) { return a * b; }

I64 Calc(I64 op, I64 a, I64 b)
{
  switch (op) {
    case 0: return Add(a, b);
    case 1: return Sub(a, b);
    case 2: return Mul(a, b);
    default: return 0;
  }
}
"%d\n", Calc(0, 10, 3);
"%d\n", Calc(1, 10, 3);
"%d\n", Calc(2, 10, 3);
