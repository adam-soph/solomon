// four_tuple.hc — 4-tuple with mixed types including U8* and F64
(I64, F64, U8 *, I64) Describe(I64 n) {
  return n, (F64)n * 1.5, "num", n * n;
}
a, b, c, d := Describe(4);
"%d %.1f %s %d\n", a, b, c, d;
a2, b2, c2, d2 := Describe(10);
"%d %.1f %s %d\n", a2, b2, c2, d2;
