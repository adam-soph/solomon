// explicit_type_args.hc — explicit vs inferred type arguments
#include <vec.hc>
T Wrap<type T>(T x) { return x; }

// Explicit
I64 r1 = Wrap<I64>(100);
F64 r2 = Wrap<F64>(2.5);
// Inferred
I64 r3 = Wrap(200);
F64 r4 = Wrap(5.0);
"%d %d\n", r1, r3;
"%.1f %.1f\n", r2, r4;

Vec<I64> v;
VecInit<I64>(&v);
VecPush<I64>(&v, 7);
VecPush<I64>(&v, 8);
"%d %d\n", VecAt<I64>(&v, 0), VecAt<I64>(&v, 1);
VecFree<I64>(&v);
