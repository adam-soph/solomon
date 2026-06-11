// divmod_unpack.hc — multi-return DivMod unpacked with :=
(I64, I64) DivMod(I64 a, I64 b) { return a/b, a%b; }
q, r := DivMod(17, 5);
"%d rem %d\n", q, r;
q2, r2 := DivMod(100, 7);
"%d rem %d\n", q2, r2;
