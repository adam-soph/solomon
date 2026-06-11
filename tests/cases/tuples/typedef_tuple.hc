// typedef_tuple.hc — typedef a tuple type and use it
typedef (I64, I64) Pair;
Pair DivMod(I64 a, I64 b) { return a/b, a%b; }
Pair Swap(Pair p) { return p[1], p[0]; }

Pair p = DivMod(13, 4);
"%d %d\n", p[0], p[1];
Pair q = Swap(p);
"%d %d\n", q[0], q[1];
