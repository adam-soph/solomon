// Functions returning F64.
F64 Average(F64 a, F64 b) { return (a + b) / 2.0; }
F64 Hyp(F64 a, F64 b)
{
  // Use only basic ops (no sqrt here).
  F64 sq = a * a + b * b;
  return sq;  // just return the sum of squares to stay deterministic
}
"%.1f\n", Average(3.0, 7.0);
"%.1f\n", Average(0.0, 10.0);
"%.1f\n", Hyp(3.0, 4.0);
