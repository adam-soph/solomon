// Function that takes fn-ptr and returns computed value.
F64 BinaryOp(F64 (*f)(F64, F64), F64 a, F64 b)
{
  return f(a, b);
}
F64 FAdd(F64 a, F64 b) { return a + b; }
F64 FSub(F64 a, F64 b) { return a - b; }
F64 FMul(F64 a, F64 b) { return a * b; }

"%.1f\n", BinaryOp(&FAdd, 2.5, 3.5);
"%.1f\n", BinaryOp(&FSub, 10.0, 4.5);
"%.1f\n", BinaryOp(&FMul, 2.0, 6.0);
