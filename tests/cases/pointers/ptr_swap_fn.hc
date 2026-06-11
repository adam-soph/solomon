// A swap function using pointer parameters.
U0 Swap(I64 *a, I64 *b) {
  I64 tmp = *a;
  *a = *b;
  *b = tmp;
}

I64 x = 10, y = 20;
Swap(&x, &y);
"%d %d\n", x, y;
