// pass_return_same.hc — pass a class to a function that returns it unchanged
class Triple { I64 a; I64 b; I64 c; };
Triple Id(Triple t) { return t; }
Triple orig; orig.a = 9; orig.b = 8; orig.c = 7;
Triple back = Id(orig);
"%d %d %d\n", back.a, back.b, back.c;
