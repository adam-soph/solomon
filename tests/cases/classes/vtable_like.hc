// vtable_like.hc — simulate vtable with function-pointer array in a class
I64 Sq(I64 x) { return x * x; }
I64 Cb(I64 x) { return x * x * x; }
I64 Dbl(I64 x) { return x * 2; }
class VTab { I64 (*ops[3])(I64); };
VTab vt;
vt.ops[0] = &Sq;
vt.ops[1] = &Cb;
vt.ops[2] = &Dbl;
"%d %d %d\n", vt.ops[0](4), vt.ops[1](3), vt.ops[2](7);
