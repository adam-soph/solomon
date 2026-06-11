// Deeply nested designated initializer.
class A { I64 v; };
class B { A inner; I64 tag; };
B b = {.tag = 99, .inner = {.v = 42}};
"%d %d\n", b.inner.v, b.tag;
