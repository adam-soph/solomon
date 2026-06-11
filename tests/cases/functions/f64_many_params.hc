// Many F64 params (exercises F64 register allocation).
F64 Dot8(F64 a, F64 b, F64 c, F64 d, F64 e, F64 f, F64 g, F64 h)
{
  return a*a + b*b + c*c + d*d + e*e + f*f + g*g + h*h;
}
"%.1f\n", Dot8(1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0);
"%.1f\n", Dot8(2.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
