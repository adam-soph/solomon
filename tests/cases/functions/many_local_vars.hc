// Function with many local variables (register pressure).
I64 Crunch(I64 n)
{
  I64 a = n, b = n+1, c = n+2, d = n+3;
  I64 e = n+4, f = n+5, g = n+6, h = n+7;
  I64 sum1 = a + b + c + d;
  I64 sum2 = e + f + g + h;
  I64 prod = (a + h) * (b + g);
  return sum1 + sum2 + prod;
}
"%d\n", Crunch(0);
"%d\n", Crunch(1);
