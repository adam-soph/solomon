// inherit_sizeof_derived.hc — sizeof derived with multiple additional fields
class Base { I64 a; };
class Child : Base { I64 b; I64 c; I64 d; };
// Base = 8, Child = 8+8+8+8 = 32
"%d %d\n", sizeof(Base), sizeof(Child);
