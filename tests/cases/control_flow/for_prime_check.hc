// Check primality with a for loop.
I64 IsPrime(I64 n)
{
  if (n < 2)
    return 0;
  I64 i;
  for (i = 2; i * i <= n; i++) {
    if (n % i == 0)
      return 0;
  }
  return 1;
}
I64 k;
for (k = 2; k <= 20; k++) {
  if (IsPrime(k))
    "%d ", k;
}
"\n";
