// Swap two globals via a helper using pointers.
I64 g_a = 10;
I64 g_b = 20;

U0 Swap(I64 *x, I64 *y) {
  I64 tmp = *x; *x = *y; *y = tmp;
}

Swap(&g_a, &g_b);
"%d %d\n", g_a, g_b;
