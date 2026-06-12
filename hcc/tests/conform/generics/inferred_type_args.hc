// inferred_type_args.hc — type arguments inferred from call site
#include <stdio.hh>
#include <vec.hh>
T Identity<type T>(T x) { return x; }
T Double<comparable T>(T a) { return a + a; }

I64 a = Identity(42);
F64 b = Identity(3.14);
U8 *c = Identity("hi");
"%d %.2f %s\n", a, b, c;

Vec<I64> v;
VecInit(&v);
VecPush(&v, 5);
VecPush(&v, 10);
"sum=%d\n", VecAt(&v,0) + VecAt(&v,1);
VecFree(&v);
