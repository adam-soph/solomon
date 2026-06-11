// pair_generic.hc — generic Pair<A,B> class with accessor functions
#include <stdlib.hc>
class Pair<type A, type B> { A first; B second; };
Pair<I64, I64> MakeII(I64 a, I64 b) { Pair<I64, I64> p; p.first = a; p.second = b; return p; }
Pair<I64, F64> MakeIF(I64 a, F64 b) { Pair<I64, F64> p; p.first = a; p.second = b; return p; }
Pair<U8 *, I64> MakeSI(U8 *a, I64 b) { Pair<U8 *, I64> p; p.first = a; p.second = b; return p; }

Pair<I64, I64> ii = MakeII(3, 7);
Pair<I64, F64> fi = MakeIF(5, 1.5);
Pair<U8 *, I64> si = MakeSI("hello", 42);
"%d %d\n", ii.first, ii.second;
"%d %.1f\n", fi.first, fi.second;
"%s %d\n", si.first, si.second;
