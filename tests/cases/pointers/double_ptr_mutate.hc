// Double pointer mutates caller's variable via a function.
I64 g_val = 0;

U0 Set(I64 **pp, I64 v) {
  *pp = &g_val;
  **pp = v;
}

I64 *ptr = NULL;
Set(&ptr, 42);
"%d\n", g_val;
"%d\n", *ptr;
