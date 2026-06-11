// decl_infer_call.hc — := infers type from function return value
I64 Double(I64 x) { return x * 2; }
F64 Half(F64 x)   { return x / 2.0; }
a := Double(21);
b := Half(7.0);
"%d %.1f\n", a, b;
