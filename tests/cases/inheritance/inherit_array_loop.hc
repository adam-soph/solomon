// inherit_array_loop.hc — array of Base* to derived instances, dispatch in loop
#define A_KIND 0
#define B_KIND 1

class Base { I64 kind; };
class A : Base { I64 av; };
class B : Base { I64 bv; };

I64 Extract(Base *b) {
  if (b->kind == A_KIND) return ((A *)b)->av;
  return ((B *)b)->bv;
}

A a0; a0.kind = A_KIND; a0.av = 5;
A a1; a1.kind = A_KIND; a1.av = 10;
B b0; b0.kind = B_KIND; b0.bv = 15;
B b1; b1.kind = B_KIND; b1.bv = 20;

Base *objs[4];
objs[0] = (Base *)&a0; objs[1] = (Base *)&a1;
objs[2] = (Base *)&b0; objs[3] = (Base *)&b1;

I64 sum = 0; I64 i;
for (i = 0; i < 4; i++) sum = sum + Extract(objs[i]);
"%d\n", sum;
