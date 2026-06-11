// fn_ptr_field.hc — class with a function-pointer field
class Op { I64 (*fn)(I64, I64); };
I64 Add(I64 a, I64 b) { return a + b; }
I64 Mul(I64 a, I64 b) { return a * b; }
Op oadd; oadd.fn = &Add;
Op omul; omul.fn = &Mul;
"%d\n", oadd.fn(3, 4);
"%d\n", omul.fn(3, 4);
