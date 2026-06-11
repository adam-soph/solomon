// inherit_upcast.hc — upcast derived pointer to base pointer, read base field
class Base { I64 id; };
class Derived : Base { I64 extra; };
Derived d; d.id = 7; d.extra = 99;
Base *b = (Base *)&d;
"%d\n", b->id;
