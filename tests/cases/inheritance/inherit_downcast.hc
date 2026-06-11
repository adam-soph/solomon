// inherit_downcast.hc — downcast from Base* back to Derived* and read derived field
class Base { I64 kind; };
class Derived : Base { I64 data; };
Derived d; d.kind = 1; d.data = 42;
Base *b = (Base *)&d;
Derived *pd = (Derived *)b;
"%d %d\n", pd->kind, pd->data;
