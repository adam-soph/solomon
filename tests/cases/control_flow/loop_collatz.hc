// Collatz sequence length.
I64 CollatzLen(I64 n)
{
  I64 steps = 0;
  while (n != 1) {
    if (n % 2 == 0)
      n /= 2;
    else
      n = 3 * n + 1;
    steps++;
  }
  return steps;
}
"%d\n", CollatzLen(6);
"%d\n", CollatzLen(27);
