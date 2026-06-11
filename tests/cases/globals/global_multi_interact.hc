// Multiple globals interacting: a running total and a call count.
I64 g_total;
I64 g_calls;

U0 Accumulate(I64 v) { g_total += v; g_calls++; }

Accumulate(5);
Accumulate(10);
Accumulate(3);
"total=%d calls=%d avg=%d\n", g_total, g_calls, g_total / g_calls;
