// class_recursion.hc — class passed through recursive function
class Pair { I64 a; I64 b; };
I64 GcdPair(Pair p) {
  if (p.b == 0) return p.a;
  Pair q; q.a = p.b; q.b = p.a % p.b;
  return GcdPair(q);
}
Pair p; p.a = 48; p.b = 36;
"%d\n", GcdPair(p);
