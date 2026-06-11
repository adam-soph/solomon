// three_unpack.hc — unpack a 3-tuple
(I64, I64, I64) Triple(I64 x) { return x, x*2, x*3; }
a, b, c := Triple(5);
"%d %d %d\n", a, b, c;
