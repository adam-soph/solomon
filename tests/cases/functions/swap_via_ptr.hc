// Swap two values via pointers.
U0 Swap(I64 *a, I64 *b)
{
  I64 tmp = *a;
  *a = *b;
  *b = tmp;
}
I64 x = 3, y = 7;
Swap(&x, &y);
"%d %d\n", x, y;
