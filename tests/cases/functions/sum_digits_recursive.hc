// Sum of digits, recursive.
I64 DigitSum(I64 n)
{
  if (n < 0) n = -n;
  if (n < 10) return n;
  return (n % 10) + DigitSum(n / 10);
}
"%d\n", DigitSum(0);
"%d\n", DigitSum(9);
"%d\n", DigitSum(123);
"%d\n", DigitSum(9999);
