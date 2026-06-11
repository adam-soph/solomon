// sizeof_generic.hc — sizeof of generic instances
class Box<type T> { T val; I64 tag; };
class Wrap<type T, int N> { T data[N]; };

// sizeof(Box<I64>) = 8+8 = 16
// sizeof(Box<F64>) = 8+8 = 16
// sizeof(Wrap<I64,4>) = 4*8 = 32
"%d\n", sizeof(Box<I64>);
"%d\n", sizeof(Box<F64>);
"%d\n", sizeof(Wrap<I64, 4>);
"%d\n", sizeof(Wrap<I64, 8>);
