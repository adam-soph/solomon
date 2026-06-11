// Deeply chained calls.
I64 F1(I64 x) { return x + 1; }
I64 F2(I64 x) { return x * 2; }
I64 F3(I64 x) { return x - 3; }
I64 Compose(I64 x) { return F1(F2(F3(x))); }
"%d\n", Compose(10);
"%d\n", Compose(Compose(5));
