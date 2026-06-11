// Function with 8 parameters (exercises the 6+ arg register boundary).
I64 Sum8(I64 a, I64 b, I64 c, I64 d, I64 e, I64 f, I64 g, I64 h)
{
  return a + b + c + d + e + f + g + h;
}
"%d\n", Sum8(1, 2, 3, 4, 5, 6, 7, 8);
"%d\n", Sum8(10, 20, 30, 40, 50, 60, 70, 80);
