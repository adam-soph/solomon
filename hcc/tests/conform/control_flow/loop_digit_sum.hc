// Sum of digits via loop.

#include <stdio.hh>
I64 DigitSum(I64 n)
{
  I64 s = 0;
  while (n > 0) {
    s += n % 10;
    n /= 10;
  }
  return s;
}
"%d\n", DigitSum(12345);
"%d\n", DigitSum(99999);
"%d\n", DigitSum(0);
