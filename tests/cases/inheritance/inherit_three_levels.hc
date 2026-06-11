// inherit_three_levels.hc — three-level hierarchy A : none, B : A, C : B
class A { I64 va; };
class B : A { I64 vb; };
class C : B { I64 vc; };
C obj; obj.va = 1; obj.vb = 2; obj.vc = 3;
"%d %d %d\n", obj.va, obj.vb, obj.vc;
