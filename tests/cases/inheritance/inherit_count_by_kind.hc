// inherit_count_by_kind.hc — count elements by kind in a heterogeneous array
class E { I64 kind; };
class Foo : E { I64 fv; };
class Bar : E { I64 bv; };

Foo f0; f0.kind = 0; f0.fv = 1;
Foo f1; f1.kind = 0; f1.fv = 2;
Bar b0; b0.kind = 1; b0.bv = 3;

E *arr[3];
arr[0] = (E *)&f0; arr[1] = (E *)&f1; arr[2] = (E *)&b0;

I64 foos = 0; I64 bars = 0; I64 i;
for (i = 0; i < 3; i++) {
  if (arr[i]->kind == 0) foos++;
  else bars++;
}
"%d %d\n", foos, bars;
