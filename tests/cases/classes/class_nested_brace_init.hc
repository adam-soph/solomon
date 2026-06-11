// class_nested_brace_init.hc — nested brace initializer
class Inner { I64 v; };
class Outer { Inner a; Inner b; };
Outer o = {{5}, {9}};
"%d %d\n", o.a.v, o.b.v;
