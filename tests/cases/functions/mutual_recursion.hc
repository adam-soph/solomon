// Mutual recursion: is_even / is_odd.
I64 IsOdd(I64 n);
I64 IsEven(I64 n)
{
  if (n == 0) return 1;
  return IsOdd(n - 1);
}
I64 IsOdd(I64 n)
{
  if (n == 0) return 0;
  return IsEven(n - 1);
}
I64 i;
for (i = 0; i <= 8; i++)
  "%d:%s ", i, IsEven(i) ? "even" : "odd";
"\n";
