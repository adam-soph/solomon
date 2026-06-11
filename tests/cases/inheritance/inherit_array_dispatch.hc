// inherit_array_dispatch.hc — array of Base* pointing at different derived instances
#define KIND_A 0
#define KIND_B 1

class Base { I64 kind; };
class TypeA : Base { I64 va; };
class TypeB : Base { I64 vb; };

I64 Val(Base *b) {
  switch (b->kind) {
    case KIND_A: return ((TypeA *)b)->va;
    case KIND_B: return ((TypeB *)b)->vb;
  }
  return -1;
}

TypeA a0; a0.kind = KIND_A; a0.va = 10;
TypeA a1; a1.kind = KIND_A; a1.va = 20;
TypeB b0; b0.kind = KIND_B; b0.vb = 30;

Base *arr[3];
arr[0] = (Base *)&a0;
arr[1] = (Base *)&a1;
arr[2] = (Base *)&b0;

I64 i;
for (i = 0; i < 3; i++) "%d\n", Val(arr[i]);
