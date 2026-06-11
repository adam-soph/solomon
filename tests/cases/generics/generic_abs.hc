// generic_abs.hc — generic Abs with if type for I64 vs F64
T Abs<comparable T>(T x) {
  if type (T is F64) return x < 0.0 ? -x : x;
  else return x < 0 ? -x : x;
}
"%d\n", Abs(-7);
"%d\n", Abs(5);
"%.1f\n", Abs(-3.5);
"%.1f\n", Abs(2.0);
