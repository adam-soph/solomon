// Global class with fields mutated by helper functions.
class Counter {
  I64 val;
  I64 limit;
};

Counter g_ctr;

U0 Init(I64 lim) { g_ctr.val = 0; g_ctr.limit = lim; }
U0 Step() { if (g_ctr.val < g_ctr.limit) g_ctr.val++; }
I64 Get() { return g_ctr.val; }

Init(3);
Step(); Step(); Step(); Step();
"%d\n", Get();
