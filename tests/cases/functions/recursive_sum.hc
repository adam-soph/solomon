// Recursive sum 1..n.
I64 Sum(I64 n)
{
  if (n <= 0) return 0;
  return n + Sum(n - 1);
}
"%d\n", Sum(10);
"%d\n", Sum(100);
