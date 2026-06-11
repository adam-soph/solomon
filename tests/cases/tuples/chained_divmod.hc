// chained_divmod.hc — use DivMod result as input to Swap
typedef (I64, I64) Pair;
Pair DivMod(I64 a, I64 b) { return a/b, a%b; }
Pair Swap(Pair p) { return p[1], p[0]; }

Pair pr = Swap(DivMod(9, 4));
"%d %d\n", pr[0], pr[1];
