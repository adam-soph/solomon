// Recursive power (fast exponentiation).
I64 Pow(I64 b, I64 e)
{
  if (e == 0) return 1;
  if (e % 2 == 0) {
    I64 half = Pow(b, e / 2);
    return half * half;
  }
  return b * Pow(b, e - 1);
}
"%d\n", Pow(2, 0);
"%d\n", Pow(2, 8);
"%d\n", Pow(3, 5);
"%d\n", Pow(5, 4);
