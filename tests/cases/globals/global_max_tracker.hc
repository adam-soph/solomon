// Global that tracks the maximum value ever seen.
I64 g_max;

U0 Observe(I64 v) { if (v > g_max) g_max = v; }

Observe(3); Observe(7); Observe(2); Observe(9); Observe(5);
"%d\n", g_max;
