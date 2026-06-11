// inherit_fn_ptr_dispatch.hc — function pointer in base, overridden by derived setup
class Op { I64 kind; I64 (*fn)(I64); };
I64 Square(I64 x) { return x * x; }
I64 Cube(I64 x)   { return x * x * x; }
class OpExt : Op { I64 bias; };
OpExt oe; oe.kind = 0; oe.fn = &Square; oe.bias = 1;
"%d\n", oe.fn(5) + oe.bias;
oe.fn = &Cube;
"%d\n", oe.fn(3) + oe.bias;
