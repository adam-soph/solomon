// Nested class positional brace initializer.
class Inner { I64 p; I64 q; };
class Outer { Inner in; I64 tag; };
Outer o = {{10, 20}, 99};
"%d %d %d\n", o.in.p, o.in.q, o.tag;
