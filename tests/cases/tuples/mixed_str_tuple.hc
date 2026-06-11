// mixed_str_tuple.hc — 3-tuple with U8* slot
(U8 *, I64, F64) Label(I64 n) { return "val", n, (F64)n / 10.0; }
name, ival, fval := Label(42);
"%s %d %.1f\n", name, ival, fval;
