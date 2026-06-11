// Recursive Fibonacci — stresses the call/return ABI and stack frames.
I64 Fib(I64 n) {
  if (n < 2) return n;
  return Fib(n - 1) + Fib(n - 2);
}
I64 r, sum = 0;
for (r = 0; r < 3; r++) sum += Fib(32);
"%d\n", sum;
