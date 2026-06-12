// inherit_deep_field.hc — three-level hierarchy, all fields accessible from bottom

#include <stdio.hh>
class A { I64 av; };
class B : A { I64 bv; };
class C : B { I64 cv; };
C c; c.av = 10; c.bv = 20; c.cv = 30;
// upcast to B*
B *bp = (B *)&c;
"%d %d\n", bp->av, bp->bv;
// upcast to A*
A *ap = (A *)&c;
"%d\n", ap->av;
