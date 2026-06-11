// double_nested.hc — doubly nested class: A inside B inside C
class A { I64 v; };
class B { A a; I64 w; };
class C { B b; I64 x; };
C c;
c.b.a.v = 1;
c.b.w   = 2;
c.x     = 3;
"%d %d %d\n", c.b.a.v, c.b.w, c.x;
