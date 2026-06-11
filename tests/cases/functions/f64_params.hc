// Functions with mixed I64/F64 params.
F64 Lerp(F64 a, F64 b, F64 t)
{
  return a + t * (b - a);
}
"%.2f\n", Lerp(0.0, 10.0, 0.5);
"%.2f\n", Lerp(0.0, 10.0, 0.0);
"%.2f\n", Lerp(0.0, 10.0, 1.0);
