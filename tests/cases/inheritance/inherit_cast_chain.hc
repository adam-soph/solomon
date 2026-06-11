// inherit_cast_chain.hc — cast C -> B -> A -> C round-trip; fields survive
class A { I64 x; };
class B : A { I64 y; };
class C : B { I64 z; };
C obj; obj.x = 1; obj.y = 2; obj.z = 3;
A *ap = (A *)&obj;
B *bp = (B *)ap;
C *cp = (C *)bp;
"%d %d %d\n", cp->x, cp->y, cp->z;
