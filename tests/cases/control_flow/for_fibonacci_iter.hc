// Iterative Fibonacci with for loop.
I64 a = 0, b = 1, i, tmp;
"%d %d ", a, b;
for (i = 2; i < 10; i++) {
  tmp = a + b;
  a = b;
  b = tmp;
  "%d ", b;
}
"\n";
