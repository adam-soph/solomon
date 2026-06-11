// pass_by_value.hc — class passed by value; callee mutates its copy, caller unchanged
class Pair { I64 a; I64 b; };
U0 Double(Pair p) { p.a = p.a * 2; p.b = p.b * 2; }
Pair q; q.a = 3; q.b = 5;
Double(q);
"%d %d\n", q.a, q.b;
