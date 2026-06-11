// Global accumulator with running product.
I64 g_product = 1;

U0 Mul(I64 v) { g_product *= v; }

Mul(2); Mul(3); Mul(5);
"%d\n", g_product;
