// tuple_equality.hc — compare slots individually after unpack
(I64, I64) Fib2(I64 n) {
  if (n <= 0) return 0, 1;
  a, b := Fib2(n-1);
  return b, a+b;
}
a, b := Fib2(7);
"%d %d\n", a, b;
