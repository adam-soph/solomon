// Ackermann function (small values only).
I64 Ack(I64 m, I64 n)
{
  if (m == 0) return n + 1;
  if (n == 0) return Ack(m - 1, 1);
  return Ack(m - 1, Ack(m, n - 1));
}
"%d\n", Ack(0, 0);
"%d\n", Ack(1, 1);
"%d\n", Ack(2, 2);
"%d\n", Ack(2, 3);
"%d\n", Ack(3, 2);
