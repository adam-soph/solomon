// inherit_sizeof.hc — sizeof derived == sizeof base + derived fields
class Base { I64 a; };
class Derived : Base { I64 b; I64 c; };
"%d %d\n", sizeof(Base), sizeof(Derived);
