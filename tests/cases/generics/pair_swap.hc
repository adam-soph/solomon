// pair_swap.hc — swap fields of a Pair<A,B>
class Pair<type A, type B> { A first; B second; };
Pair<B, A> PairSwap<type A, type B>(Pair<A, B> p) {
  Pair<B, A> q;
  q.first = p.second;
  q.second = p.first;
  return q;
}
Pair<I64, F64> p;
p.first = 7; p.second = 3.14;
Pair<F64, I64> q = PairSwap(p);
"%.2f %d\n", q.first, q.second;
