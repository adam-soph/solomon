// tuple_loop.hc — unpack in a loop
(I64, I64) Pair2(I64 i) { return i, i*i; }
I64 k;
for (k = 1; k <= 5; k++) {
  x, xsq := Pair2(k);
  "%d^2=%d\n", x, xsq;
}
