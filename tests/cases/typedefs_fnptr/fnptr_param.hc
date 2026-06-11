// Function pointer passed as a parameter and called inside.
I64 Apply(I64 (*f)(I64, I64), I64 a, I64 b)
{
  return f(a, b);
}

I64 Add(I64 a, I64 b) { return a + b; }
I64 Mul(I64 a, I64 b) { return a * b; }

"%d\n", Apply(&Add, 3, 4);
"%d\n", Apply(&Mul, 3, 4);
