// Function with mixed I64 and F64 params across the register boundary.
F64 Weighted(I64 n, F64 w, I64 m, F64 v, I64 k, F64 u, I64 j, F64 t)
{
  return (F64)n * w + (F64)m * v + (F64)k * u + (F64)j * t;
}
"%.1f\n", Weighted(1, 1.0, 2, 2.0, 3, 3.0, 4, 4.0);
