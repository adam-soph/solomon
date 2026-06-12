// inherit_switch_kind.hc — full switch over a kind field, 5 cases

#include <stdio.hh>
class Ev { I64 kind; };
class Ev0 : Ev { I64 v; };
class Ev1 : Ev { I64 v; };
class Ev2 : Ev { I64 v; };
class Ev3 : Ev { I64 v; };
class Ev4 : Ev { I64 v; };

I64 Handle(Ev *e) {
  switch (e->kind) {
    case 0: return ((Ev0 *)e)->v * 1;
    case 1: return ((Ev1 *)e)->v * 2;
    case 2: return ((Ev2 *)e)->v * 3;
    case 3: return ((Ev3 *)e)->v * 4;
    case 4: return ((Ev4 *)e)->v * 5;
  }
  return 0;
}

Ev0 e0; e0.kind = 0; e0.v = 1;
Ev1 e1; e1.kind = 1; e1.v = 2;
Ev2 e2; e2.kind = 2; e2.v = 3;
Ev3 e3; e3.kind = 3; e3.v = 4;
Ev4 e4; e4.kind = 4; e4.v = 5;

Ev *arr[5];
arr[0] = (Ev *)&e0; arr[1] = (Ev *)&e1; arr[2] = (Ev *)&e2;
arr[3] = (Ev *)&e3; arr[4] = (Ev *)&e4;

I64 i;
for (i = 0; i < 5; i++) "%d\n", Handle(arr[i]);
