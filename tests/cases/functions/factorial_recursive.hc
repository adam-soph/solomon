// Recursive factorial.
I64 Fact(I64 n)
{
  if (n <= 1) return 1;
  return n * Fact(n - 1);
}
I64 i;
for (i = 0; i <= 10; i++)
  "%d! = %d\n", i, Fact(i);
