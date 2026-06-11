// nested_unpack.hc — multiple tuple unpack calls in sequence
(I64, I64) MinMax(I64 a, I64 b) {
  if (a <= b) return a, b;
  return b, a;
}
mn, mx := MinMax(7, 3);
"%d %d\n", mn, mx;
mn2, mx2 := MinMax(1, 9);
"%d %d\n", mn2, mx2;
mn3, mx3 := MinMax(5, 5);
"%d %d\n", mn3, mx3;
