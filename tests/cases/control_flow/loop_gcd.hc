// GCD via a while loop (Euclidean algorithm).
I64 GCD(I64 a, I64 b)
{
  while (b != 0) {
    I64 t = b;
    b = a % b;
    a = t;
  }
  return a;
}
"%d\n", GCD(48, 18);
"%d\n", GCD(100, 75);
"%d\n", GCD(17, 5);
