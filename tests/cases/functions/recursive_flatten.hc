// Recursive function counting down and building up (stack frame exercise).
U0 Countdown(I64 n)
{
  if (n < 0) {
    "liftoff\n";
    return;
  }
  "%d\n", n;
  Countdown(n - 1);
}
Countdown(4);
