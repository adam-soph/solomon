// fib.hc — functions, recursion, and loops.
I64 Fib(I64 n)
{
  if (n < 2)
    return n;
  return Fib(n - 1) + Fib(n - 2);
}

U0 Main()
{
  I64 i;
  for (i = 0; i < 10; i++)
    "%d ", Fib(i);
  '\n';

  // A while loop summing the first few squares.
  I64 sum = 0, k = 1;
  while (k <= 5) {
    sum += k * k;
    k++;
  }
  "sum of squares = %d\n", sum;
}
