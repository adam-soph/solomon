// Count digits using do-while (handles n=0 correctly).
I64 CountDigits(I64 n)
{
  I64 count = 0;
  do {
    n /= 10;
    count++;
  } while (n > 0);
  return count;
}
"%d\n", CountDigits(0);
"%d\n", CountDigits(5);
"%d\n", CountDigits(999);
"%d\n", CountDigits(12345678);
